use super::*;
use crate::{backup, json::hash, time};
use serde::Serialize;

const HISTORY: &str = "CREATE TABLE IF NOT EXISTS rust_schema_migrations(version INTEGER PRIMARY KEY,checksum TEXT NOT NULL,applied_at TEXT NOT NULL)";
const LATEST: i64 = 8;
#[derive(Serialize)]
pub struct Inspection {
    pub schema_version: i64,
    pub target_version: i64,
    pub pending: Vec<i64>,
    pub existing: bool,
}
fn checksum(version: i64) -> String {
    // v0.10.0/v0.10.1 included legacy.rs source bytes in this checksum. Preserve
    // that released value across implementation-only refactors; schema changes
    // must use a new migration instead of rewriting this historical contract.
    if version == 1 {
        return "2f7d2bb503201536c4f0b51471b56dec24fa3e4a43eee98f777ef55e6f2ee176".into();
    }
    let bytes = match version {
        2 => include_str!("../sync_schema.sql"),
        3 => include_str!("../security_schema.sql"),
        4 => include_str!("../payment_schema.sql"),
        5 => include_str!("../deletion_scope_schema.sql"),
        6 => include_str!("../reminder_schema.sql"),
        7 => include_str!("../operations_schema.sql"),
        8 => include_str!("../vendor_schema.sql"),
        _ => "",
    };
    hash(bytes.as_bytes())
}
async fn applied(db: &mut SqliteConnection) -> Result<i64> {
    if optional(
        db,
        "SELECT name FROM sqlite_master WHERE type='table' AND name='rust_schema_migrations'",
        &[],
    )
    .await?
    .is_none()
    {
        return Ok(0);
    }
    let versions = all(
        db,
        "SELECT version,checksum FROM rust_schema_migrations ORDER BY version",
        &[],
    )
    .await?;
    let mut last = 0;
    for row in versions {
        let version = row["version"].as_i64().unwrap_or(0);
        if version != last + 1 || version > LATEST || row["checksum"] != checksum(version) {
            return Err(Error::conflict(
                "账本迁移版本或校验值不兼容，请保留账本并使用匹配版本",
            ));
        }
        last = version;
    }
    Ok(last)
}
pub async fn inspect(path: &Path) -> Result<Inspection> {
    if !path.exists() || std::fs::metadata(path)?.len() == 0 {
        return Ok(Inspection {
            schema_version: 0,
            target_version: LATEST,
            pending: (1..=LATEST).collect(),
            existing: false,
        });
    }
    backup::check(path).await?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .read_only(true)
                .create_if_missing(false)
                .busy_timeout(Duration::from_secs(15)),
        )
        .await?;
    let result = async {
        let mut db = pool.acquire().await?;
        let version = applied(&mut db).await?;
        let existing = optional(
            &mut db,
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' LIMIT 1",
            &[],
        )
        .await?
        .is_some();
        Ok(Inspection {
            schema_version: version,
            target_version: LATEST,
            pending: ((version + 1)..=LATEST).collect(),
            existing,
        })
    }
    .await;
    pool.close().await;
    result
}
pub(super) async fn protect(path: &Path) -> Result<()> {
    let check = inspect(path).await?;
    if check.existing && !check.pending.is_empty() {
        let directory = path
            .parent()
            .ok_or_else(Error::unavailable)?
            .join("backups");
        std::fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        }
        backup::create(
            path,
            &directory.join(format!(
                "before-schema-{LATEST}-{}.sqlite3",
                uuid::Uuid::new_v4()
            )),
        )
        .await?;
    }
    Ok(())
}
pub(super) async fn apply(db: &mut SqliteConnection) -> Result<()> {
    let previous = applied(db).await?;
    sqlx::raw_sql(HISTORY).execute(&mut *db).await?;
    for version in (previous + 1)..=LATEST {
        match version {
            1 => {
                sqlx::raw_sql(SCHEMA).execute(&mut *db).await?;
                super::legacy::migrate(db).await?;
            }
            2 => {
                sqlx::raw_sql(include_str!("../sync_schema.sql"))
                    .execute(&mut *db)
                    .await?;
            }
            3 => {
                sqlx::raw_sql(include_str!("../security_schema.sql"))
                    .execute(&mut *db)
                    .await?;
            }
            4 => {
                sqlx::raw_sql(include_str!("../payment_schema.sql"))
                    .execute(&mut *db)
                    .await?;
            }
            5 => {
                sqlx::raw_sql(include_str!("../deletion_scope_schema.sql"))
                    .execute(&mut *db)
                    .await?;
            }
            6 => {
                sqlx::raw_sql(include_str!("../reminder_schema.sql"))
                    .execute(&mut *db)
                    .await?;
            }
            7 => {
                sqlx::raw_sql(include_str!("../operations_schema.sql"))
                    .execute(&mut *db)
                    .await?;
            }
            8 => {
                sqlx::raw_sql(include_str!("../vendor_schema.sql"))
                    .execute(&mut *db)
                    .await?;
                super::legacy::vendor_columns(db).await?;
            }
            _ => unreachable!(),
        }
        exec(
            db,
            "INSERT INTO rust_schema_migrations(version,checksum,applied_at) VALUES(?,?,?)",
            &[json!(version), json!(checksum(version)), json!(time::now())],
        )
        .await?;
    }
    let integrity = all(db, "PRAGMA foreign_key_check", &[]).await?;
    if !integrity.is_empty() {
        return Err(Error::conflict("迁移后的账本外键检查未通过"));
    }
    Ok(())
}
