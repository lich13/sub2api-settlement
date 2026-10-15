use crate::{
    error::{Error, Result},
    json::dumps,
};
use futures_util::TryStreamExt;
use serde_json::{Value, json};
use sqlx::{
    Column, Row, SqliteConnection, SqlitePool, TypeInfo, ValueRef,
    sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
    },
};
use std::{path::Path, time::Duration};

pub const SCHEMA: &str = include_str!("schema.sql");
#[derive(Clone)]
pub struct Store {
    pub reads: SqlitePool,
    pub writes: SqlitePool,
    pub path: std::path::PathBuf,
}
impl Store {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(15));
        let writes = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(15))
            .connect_with(options.clone())
            .await?;
        let mut conn = writes.acquire().await?;
        sqlx::raw_sql(SCHEMA).execute(&mut *conn).await?;
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        match migrate(&mut conn).await {
            Ok(()) => {
                sqlx::query("COMMIT").execute(&mut *conn).await?;
            }
            Err(e) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                return Err(e);
            }
        }
        drop(conn);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let reads = SqlitePoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(15))
            .connect_with(options.create_if_missing(false).read_only(true))
            .await?;
        Ok(Self {
            reads,
            writes,
            path: path.to_owned(),
        })
    }
    pub async fn write(&self) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>> {
        Ok(self.writes.begin_with("BEGIN IMMEDIATE").await?)
    }
    pub async fn read(&self) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>> {
        Ok(self.reads.begin().await?)
    }
    pub async fn close(&self) {
        self.reads.close().await;
        self.writes.close().await;
    }
}
pub fn row(r: &SqliteRow) -> Result<Value> {
    let mut out = serde_json::Map::new();
    for (index, column) in r.columns().iter().enumerate() {
        let raw = r.try_get_raw(index)?;
        let value = if raw.is_null() {
            Value::Null
        } else {
            match raw.type_info().name() {
                "INTEGER" => json!(r.try_get::<i64, _>(index)?),
                "REAL" => json!(r.try_get::<f64, _>(index)?),
                _ => json!(r.try_get::<String, _>(index)?),
            }
        };
        out.insert(column.name().to_owned(), value);
    }
    Ok(Value::Object(out))
}
fn query<'a>(
    sql: &'a str,
    args: &'a [Value],
) -> sqlx::query::Query<'a, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'a>> {
    let mut q = sqlx::query(sql);
    for arg in args {
        q = match arg {
            Value::Null => q.bind(Option::<String>::None),
            Value::Bool(b) => q.bind(i64::from(*b)),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    q.bind(i)
                } else {
                    q.bind(n.as_f64().unwrap_or(0.0))
                }
            }
            Value::String(s) => q.bind(s.as_str()),
            _ => q.bind(dumps(arg)),
        };
    }
    q
}
pub async fn all(db: &mut SqliteConnection, sql: &str, args: &[Value]) -> Result<Vec<Value>> {
    let mut stream = query(sql, args).fetch(db);
    let mut rows = Vec::new();
    while let Some(item) = stream.try_next().await? {
        rows.push(row(&item)?);
    }
    Ok(rows)
}
pub async fn optional(
    db: &mut SqliteConnection,
    sql: &str,
    args: &[Value],
) -> Result<Option<Value>> {
    query(sql, args)
        .fetch_optional(db)
        .await?
        .as_ref()
        .map(row)
        .transpose()
}
pub async fn one(
    db: &mut SqliteConnection,
    sql: &str,
    args: &[Value],
    message: &str,
) -> Result<Value> {
    optional(db, sql, args)
        .await?
        .ok_or_else(|| Error::new(404, message))
}
pub async fn exec(
    db: &mut SqliteConnection,
    sql: &str,
    args: &[Value],
) -> Result<sqlx::sqlite::SqliteQueryResult> {
    Ok(query(sql, args).execute(db).await?)
}
pub async fn meta(db: &mut SqliteConnection, key: &str) -> Result<Value> {
    let r = optional(db, "SELECT value FROM meta WHERE key=?", &[json!(key)]).await?;
    Ok(match r {
        Some(v) => serde_json::from_str(v["value"].as_str().unwrap_or("null"))?,
        None => Value::Null,
    })
}
pub async fn put(db: &mut SqliteConnection, key: &str, value: &Value) -> Result<()> {
    exec(
        db,
        "INSERT INTO meta VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        &[json!(key), json!(dumps(value))],
    )
    .await?;
    Ok(())
}
pub async fn revision(db: &mut SqliteConnection, root: i64) -> Result<i64> {
    Ok(optional(
        db,
        "SELECT version FROM rust_root_versions WHERE root_id=?",
        &[json!(root)],
    )
    .await?
    .map(|v| v["version"].as_i64().unwrap_or(0))
    .unwrap_or(0))
}
async fn migrate(db: &mut SqliteConnection) -> Result<()> {
    let columns = all(db, "PRAGMA table_info(cycles)", &[]).await?;
    let additions = [
        ("start_mode", "TEXT NOT NULL DEFAULT 'exact'"),
        ("start_lower", "TEXT"),
        ("start_upper", "TEXT"),
        ("start_confirmed", "INTEGER NOT NULL DEFAULT 1"),
        ("start_confirmed_by", "INTEGER"),
        ("start_confirmed_at", "TEXT"),
        ("start_source_usage_id", "INTEGER"),
        ("renewed_from_cycle_id", "INTEGER"),
        ("start_anchor", "TEXT"),
        ("cost_kind", "TEXT NOT NULL DEFAULT 'exact'"),
        ("cost_source", "TEXT NOT NULL DEFAULT 'manual'"),
        ("bill_import_id", "INTEGER"),
        ("bill_end", "TEXT"),
        ("bill_digest", "TEXT"),
        ("bill_difference_confirmed", "INTEGER NOT NULL DEFAULT 0"),
        (
            "bill_allocation_mode",
            "TEXT NOT NULL DEFAULT 'proportional'",
        ),
        ("billable_cost_fen", "INTEGER"),
        ("start_locked", "INTEGER NOT NULL DEFAULT 0"),
    ];
    for (name, kind) in additions {
        if !columns.iter().any(|r| r["name"] == name) {
            exec(
                db,
                &format!("ALTER TABLE cycles ADD COLUMN {name} {kind}"),
                &[],
            )
            .await?;
        }
    }
    exec(
        db,
        "UPDATE cycles SET start_mode='exact' WHERE start_mode IS NULL OR start_mode=''",
        &[],
    )
    .await?;
    exec(
        db,
        "UPDATE cycles SET start_lower=start WHERE start_lower IS NULL",
        &[],
    )
    .await?;
    exec(
        db,
        "UPDATE cycles SET start_confirmed=1 WHERE start_confirmed IS NULL",
        &[],
    )
    .await?;
    for table in ["settlements", "orders"] {
        let columns = all(db, &format!("PRAGMA table_info({table})"), &[]).await?;
        if columns
            .iter()
            .any(|r| r["name"] == "cycle_id" && r["notnull"] == 1)
        {
            let sequence = optional(
                db,
                "SELECT seq FROM sqlite_sequence WHERE name=?",
                &[json!(table)],
            )
            .await?;
            let statement = SCHEMA
                .split(';')
                .map(str::trim)
                .find(|s| s.starts_with(&format!("CREATE TABLE IF NOT EXISTS {table}(")))
                .ok_or_else(Error::unavailable)?;
            exec(
                db,
                &statement.replace(&format!("{table}("), &format!("{table}_v4(")),
                &[],
            )
            .await?;
            let names = columns
                .iter()
                .map(|r| r["name"].as_str().unwrap())
                .collect::<Vec<_>>()
                .join(",");
            exec(
                db,
                &format!("INSERT INTO {table}_v4({names}) SELECT {names} FROM {table}"),
                &[],
            )
            .await?;
            exec(db, &format!("DROP TABLE {table}"), &[]).await?;
            exec(
                db,
                &format!("ALTER TABLE {table}_v4 RENAME TO {table}"),
                &[],
            )
            .await?;
            if let Some(seq) = sequence {
                exec(
                    db,
                    "UPDATE sqlite_sequence SET seq=max(seq,?) WHERE name=?",
                    &[seq["seq"].clone(), json!(table)],
                )
                .await?;
            }
        } else if !columns.iter().any(|r| r["name"] == "expense_id") {
            exec(
                db,
                &format!("ALTER TABLE {table} ADD COLUMN expense_id INTEGER"),
                &[],
            )
            .await?;
        }
    }
    if !all(db, "PRAGMA table_info(orders)", &[])
        .await?
        .iter()
        .any(|r| r["name"] == "netting_payment_id")
    {
        exec(db,"ALTER TABLE orders ADD COLUMN netting_payment_id INTEGER REFERENCES netting_payments(id)",&[]).await?;
    }
    sqlx::raw_sql("CREATE INDEX IF NOT EXISTS order_payer ON orders(payer_id,status); CREATE INDEX IF NOT EXISTS order_creditor ON orders(creditor_id,status); CREATE INDEX IF NOT EXISTS order_netting_payment ON orders(netting_payment_id); CREATE TABLE IF NOT EXISTS rust_root_versions(root_id INTEGER PRIMARY KEY, version INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS rust_sync_stage(id INTEGER PRIMARY KEY,account_id INTEGER NOT NULL,root_id INTEGER NOT NULL,user_id INTEGER NOT NULL,amount TEXT NOT NULL,created_at TEXT NOT NULL,detail TEXT); CREATE INDEX IF NOT EXISTS rust_stage_root ON rust_sync_stage(root_id,created_at); CREATE TABLE IF NOT EXISTS rust_source_presence(id INTEGER PRIMARY KEY); CREATE TABLE IF NOT EXISTS rust_sync_changed(root_id INTEGER PRIMARY KEY); CREATE INDEX IF NOT EXISTS usage_account_time ON usage(account_id,created_at); CREATE INDEX IF NOT EXISTS cycle_open ON cycles(status,account_id);").execute(&mut *db).await?;
    Ok(())
}
