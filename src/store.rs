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
mod legacy;
pub mod migrations;
#[derive(Clone)]
pub struct Store {
    pub reads: SqlitePool,
    pub writes: SqlitePool,
    pub path: std::path::PathBuf,
}
impl Store {
    pub async fn open(path: &Path) -> Result<Self> {
        migrations::protect(path).await?;
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
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        match migrations::apply(&mut conn).await {
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
) -> Result<sqlx::query::Query<'a, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'a>>> {
    let mut q = sqlx::query(sql);
    for arg in args {
        q = match arg {
            Value::Null => q.bind(Option::<String>::None),
            Value::Bool(b) => q.bind(i64::from(*b)),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    q.bind(i)
                } else {
                    let number = n
                        .as_f64()
                        .filter(|value| value.is_finite())
                        .ok_or_else(|| {
                            tracing::error!("database numeric argument cannot be represented");
                            Error::unavailable()
                        })?;
                    // Financial values are integer fen before reaching this
                    // generic binder. REAL remains necessary for session epochs.
                    debug_assert!(number.is_finite());
                    q.bind(number)
                }
            }
            Value::String(s) => q.bind(s.as_str()),
            _ => q.bind(dumps(arg)),
        };
    }
    Ok(q)
}
pub async fn all(db: &mut SqliteConnection, sql: &str, args: &[Value]) -> Result<Vec<Value>> {
    let mut stream = query(sql, args)?.fetch(db);
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
    query(sql, args)?
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
    Ok(query(sql, args)?.execute(db).await?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn numeric_arguments_preserve_integer_and_real_bindings() {
        use sqlx::Connection;
        let mut db = SqliteConnection::connect(":memory:").await.unwrap();
        for number in [i64::MIN, 0, 1, i64::MAX] {
            let row = one(
                &mut db,
                "SELECT ? AS value, typeof(?) AS kind",
                &[json!(number), json!(number)],
                "missing",
            )
            .await
            .unwrap();
            assert_eq!(row["value"], json!(number));
            assert_eq!(row["kind"], "integer");
        }
        for number in [0.125, -0.5, 1_800_000_000.125, 1e308] {
            let row = one(
                &mut db,
                "SELECT ? AS value, typeof(?) AS kind",
                &[json!(number), json!(number)],
                "missing",
            )
            .await
            .unwrap();
            assert_eq!(row["value"].as_f64(), Some(number));
            assert_eq!(row["kind"], "real");
        }
        // Preserve the existing generic binder's unsigned-overflow conversion;
        // monetary inputs are independently bounded to i64 before binding.
        let row = one(
            &mut db,
            "SELECT ? AS value, typeof(?) AS kind",
            &[json!(u64::MAX), json!(u64::MAX)],
            "missing",
        )
        .await
        .unwrap();
        assert_eq!(row["value"].as_f64(), Some(u64::MAX as f64));
        assert_eq!(row["kind"], "real");
        db.close().await.unwrap();
    }

    #[test]
    fn invalid_nonfinite_numbers_are_rejected_before_binding() {
        // The current serde_json feature set cannot represent these as Numbers.
        for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(serde_json::Number::from_f64(number).is_none());
        }
        assert!(serde_json::from_str::<Value>("1e400").is_err());
        assert!(serde_json::from_str::<Value>("NaN").is_err());
    }
}
