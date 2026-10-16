#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use settlement::{
    backup,
    store::{Store, migrations},
};
use sqlx::{ConnectOptions, Connection, Row, sqlite::SqliteConnectOptions};
use tempfile::TempDir;

const LEGACY_SQL: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/ledger/legacy-written.sql"
);

// Fixed from the published v0.10.1 migration history, independently of the
// checksum implementation under test.
const PUBLISHED_MIGRATION_CHECKSUMS: [&str; 7] = [
    "2f7d2bb503201536c4f0b51471b56dec24fa3e4a43eee98f777ef55e6f2ee176",
    "30d331e26fec4786252dbd5ba5353b477002a943ba971c3eb7765f2a36e46ac0",
    "ff99cd10deeb058026124228ac96d6cc333948bbeb590c09e4bfe77e33f5954d",
    "8c27e69c2df11b7cb062e12ab71ec605f2ab2cc96476fa15e01481f16b839fe2",
    "c6318f749e4276b2774c258a81dc31a3ca8f704b033d0fd822fe189ec9853d2b",
    "8c11465571dfeb251884247febafc2852b4a292364e496afe69393a9caebec54",
    "f6a9788b9fb5818fef31c4866aa8716d43f9d2ecd637c09129658a45c5efb53b",
];

#[derive(Debug, PartialEq)]
struct DatabaseSnapshot {
    schema: Vec<(String, String, Option<String>)>,
    tables: Vec<(String, Vec<Value>)>,
    migrations: Vec<(i64, String, String)>,
}

async fn connect(path: &Path) -> sqlx::SqliteConnection {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(false)
        .connect()
        .await
        .expect("open temporary SQLite database")
}

async fn connect_read_only(path: &Path) -> sqlx::SqliteConnection {
    SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false)
        .connect()
        .await
        .expect("open temporary SQLite database read-only")
}

async fn create_simple_source(path: &Path) {
    let mut db = connect(path).await;
    sqlx::raw_sql("CREATE TABLE sample(id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO sample VALUES(1, 'kept');")
        .execute(&mut db)
        .await
        .expect("create simple backup source");
    db.close().await.expect("close simple backup source");
}

async fn import_legacy_fixture(path: &Path) {
    let sql = fs::read_to_string(LEGACY_SQL).expect("read existing legacy SQL fixture");
    let mut db = connect(path).await;
    sqlx::raw_sql(&sql)
        .execute(&mut db)
        .await
        .expect("import existing legacy SQL fixture");
    db.close().await.expect("close imported legacy ledger");
}

fn backup_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = fs::read_dir(directory)
        .expect("read backup directory")
        .map(|entry| entry.expect("read backup entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "sqlite3")
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn run_migrate_check(data_dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_settlement"))
        .env_clear()
        .env("DATA_DIR", data_dir)
        .env("SETTLEMENT_ENV", "test")
        .env("SOURCE_DATABASE_URL", "")
        .args(["migrate", "--check"])
        .output()
        .expect("run actual settlement migrate --check command")
}

fn parse_json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected JSON stdout, got {:?}: {error}; stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn database_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

async fn database_snapshot(path: &Path) -> DatabaseSnapshot {
    let mut db = connect_read_only(path).await;
    let schema = sqlx::query(
        "SELECT type,name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
    )
    .fetch_all(&mut db)
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        (
            row.try_get::<String, _>("type").unwrap(),
            row.try_get::<String, _>("name").unwrap(),
            row.try_get::<Option<String>, _>("sql").unwrap(),
        )
    })
    .collect();

    let table_names = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    let mut tables = Vec::with_capacity(table_names.len());
    for table in table_names {
        let quoted_table = table.replace('"', "\"\"");
        let rows = sqlx::query(&format!("SELECT * FROM \"{quoted_table}\" ORDER BY rowid"))
            .fetch_all(&mut db)
            .await
            .unwrap();
        let values = rows
            .iter()
            .map(|row| settlement::store::row(row).expect("serialize ledger row"))
            .collect();
        tables.push((table, values));
    }

    let migrations = sqlx::query(
        "SELECT version,checksum,applied_at FROM rust_schema_migrations ORDER BY version",
    )
    .fetch_all(&mut db)
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        (
            row.try_get::<i64, _>("version").unwrap(),
            row.try_get::<String, _>("checksum").unwrap(),
            row.try_get::<String, _>("applied_at").unwrap(),
        )
    })
    .collect();
    db.close().await.unwrap();

    DatabaseSnapshot {
        schema,
        tables,
        migrations,
    }
}

async fn create_published_ledger(path: &Path) {
    import_legacy_fixture(path).await;
    let store = Store::open(path)
        .await
        .expect("prepare current ledger schema for published history");
    store.close().await;

    let mut db = connect(path).await;
    for (index, checksum) in PUBLISHED_MIGRATION_CHECKSUMS.iter().enumerate() {
        let result = sqlx::query("UPDATE rust_schema_migrations SET checksum=? WHERE version=?")
            .bind(*checksum)
            .bind(index as i64 + 1)
            .execute(&mut db)
            .await
            .unwrap();
        assert_eq!(
            result.rows_affected(),
            1,
            "published version {} exists",
            index + 1
        );
    }
    db.close().await.unwrap();
}

fn published_migration_records() -> Vec<(i64, String)> {
    PUBLISHED_MIGRATION_CHECKSUMS
        .iter()
        .enumerate()
        .map(|(index, checksum)| (index as i64 + 1, (*checksum).to_owned()))
        .collect()
}

#[tokio::test]
async fn backup_includes_committed_wal_pages_but_excludes_an_active_transaction() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source.sqlite3");
    let output = temporary.path().join("snapshot.sqlite3");
    let mut writer = SqliteConnectOptions::new()
        .filename(&source)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(false)
        .connect()
        .await
        .expect("open WAL source");
    sqlx::raw_sql(
        "PRAGMA wal_autocheckpoint=0; CREATE TABLE sample(id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO sample VALUES(1, 'committed');",
    )
    .execute(&mut writer)
    .await
    .expect("write committed WAL data");
    let wal = database_sidecar(&source, "-wal");
    assert!(
        fs::metadata(&wal).unwrap().len() > 0,
        "committed pages remain in WAL"
    );

    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut writer)
        .await
        .unwrap();
    sqlx::query("INSERT INTO sample VALUES(2, 'uncommitted')")
        .execute(&mut writer)
        .await
        .unwrap();
    let active_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM sample")
        .fetch_one(&mut writer)
        .await
        .unwrap();
    assert_eq!(active_rows, 2);

    backup::create(&source, &output)
        .await
        .expect("backup active WAL database");
    let mut snapshot = connect_read_only(&output).await;
    let rows = sqlx::query("SELECT id, value FROM sample ORDER BY id")
        .fetch_all(&mut snapshot)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].try_get::<i64, _>("id").unwrap(), 1);
    assert_eq!(rows[0].try_get::<String, _>("value").unwrap(), "committed");
    snapshot.close().await.unwrap();
    writer.close().await.unwrap();
}

#[tokio::test]
async fn backup_rejects_existing_destination_without_changing_it() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source.sqlite3");
    let output = temporary.path().join("existing.sqlite3");
    create_simple_source(&source).await;
    fs::write(&output, b"preserve existing destination bytes").unwrap();

    let error = backup::create(&source, &output)
        .await
        .expect_err("existing backup destination must be refused");
    assert_eq!(error.status, 409);
    assert_eq!(
        fs::read(&output).unwrap(),
        b"preserve existing destination bytes"
    );
}

#[tokio::test]
async fn integrity_and_foreign_key_failures_do_not_publish_backup_files() {
    let temporary = TempDir::new().unwrap();
    let integrity_source = temporary.path().join("integrity.sqlite3");
    let integrity_output = temporary.path().join("integrity-backup.sqlite3");
    let mut db = connect(&integrity_source).await;
    sqlx::raw_sql(
        "CREATE TABLE checked(value INTEGER CHECK(value > 0)); PRAGMA ignore_check_constraints=ON; INSERT INTO checked VALUES(-1);",
    )
    .execute(&mut db)
    .await
    .unwrap();
    db.close().await.unwrap();
    let integrity_error = backup::create(&integrity_source, &integrity_output)
        .await
        .expect_err("integrity-check failure must refuse publication");
    assert!(integrity_error.message.contains("完整性"));
    assert!(!integrity_output.exists());

    let foreign_key_source = temporary.path().join("foreign-key.sqlite3");
    let foreign_key_output = temporary.path().join("foreign-key-backup.sqlite3");
    let mut db = connect(&foreign_key_source).await;
    sqlx::raw_sql(
        "CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(parent_id INTEGER REFERENCES parents(id)); INSERT INTO children VALUES(99);",
    )
    .execute(&mut db)
    .await
    .unwrap();
    db.close().await.unwrap();
    let foreign_key_error = backup::create(&foreign_key_source, &foreign_key_output)
        .await
        .expect_err("foreign-key failure must refuse publication");
    assert!(foreign_key_error.message.contains("外键"));
    assert!(!foreign_key_output.exists());

    assert!(fs::read_dir(temporary.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".backup-")
    }));
}

#[tokio::test]
async fn concurrent_backups_to_one_destination_publish_exactly_one_copy() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source.sqlite3");
    let output = temporary.path().join("snapshot.sqlite3");
    let mut db = connect(&source).await;
    sqlx::raw_sql(
        "CREATE TABLE payload(id INTEGER PRIMARY KEY, value BLOB NOT NULL); INSERT INTO payload VALUES(1, zeroblob(1048576));",
    )
    .execute(&mut db)
    .await
    .unwrap();
    db.close().await.unwrap();

    let (first, second) = tokio::join!(
        backup::create(&source, &output),
        backup::create(&source, &output)
    );
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "exactly one call may publish"
    );
    assert!(output.is_file());
    backup::check(&output)
        .await
        .expect("published copy remains valid");
    let temporary_names = fs::read_dir(temporary.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".backup-"))
        .collect::<Vec<_>>();
    assert!(
        temporary_names.is_empty(),
        "temporary files were cleaned: {temporary_names:?}"
    );
}

#[tokio::test]
async fn first_migration_keeps_a_private_pre_migration_backup_and_reopen_adds_none() {
    let temporary = TempDir::new().unwrap();
    let ledger = temporary.path().join("settlement.sqlite3");
    import_legacy_fixture(&ledger).await;

    let store = Store::open(&ledger).await.expect("migrate legacy ledger");
    store.close().await;

    let backup_directory = temporary.path().join("backups");
    let backups = backup_files(&backup_directory);
    assert_eq!(backups.len(), 1);
    #[cfg(unix)]
    {
        assert_eq!(
            fs::metadata(&backups[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&backup_directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    let mut pre_migration = connect_read_only(&backups[0]).await;
    let migration_table: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='rust_schema_migrations'",
    )
    .fetch_one(&mut pre_migration)
    .await
    .unwrap();
    let original_accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(&mut pre_migration)
        .await
        .unwrap();
    assert_eq!(migration_table, 0, "backup must precede schema migrations");
    assert_eq!(original_accounts, 7, "backup preserves legacy ledger rows");
    pre_migration.close().await.unwrap();

    let mut migrated = connect_read_only(&ledger).await;
    let version: i64 = sqlx::query_scalar("SELECT max(version) FROM rust_schema_migrations")
        .fetch_one(&mut migrated)
        .await
        .unwrap();
    migrated.close().await.unwrap();
    let target_version = migrations::inspect(&ledger).await.unwrap().target_version;
    assert_eq!(version, target_version);

    let store = Store::open(&ledger).await.expect("reopen migrated ledger");
    store.close().await;
    assert_eq!(backup_files(&backup_directory), backups);
}

#[tokio::test]
async fn migrate_check_on_missing_data_directory_is_read_only_and_creates_nothing() {
    let temporary = TempDir::new().unwrap();
    let data_dir = temporary.path().join("not-created").join("data");
    let expected = migrations::inspect(&data_dir.join("settlement.sqlite3"))
        .await
        .unwrap();

    let output = run_migrate_check(&data_dir);
    assert!(
        output.status.success(),
        "migrate --check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_json_stdout(&output);
    assert_eq!(report["schema_version"], 0);
    assert_eq!(report["target_version"], expected.target_version);
    assert_eq!(report["pending"], serde_json::json!(expected.pending));
    assert_eq!(report["existing"], false);
    assert!(!data_dir.exists());
}

#[tokio::test]
async fn migrate_check_reports_pending_versions_without_changing_legacy_database() {
    let temporary = TempDir::new().unwrap();
    let data_dir = temporary.path().join("data");
    fs::create_dir(&data_dir).unwrap();
    let ledger = data_dir.join("settlement.sqlite3");
    import_legacy_fixture(&ledger).await;
    let before = fs::read(&ledger).unwrap();
    let expected = migrations::inspect(&ledger).await.unwrap();

    let output = run_migrate_check(&data_dir);
    assert!(
        output.status.success(),
        "migrate --check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_json_stdout(&output);
    assert_eq!(report["schema_version"], 0);
    assert_eq!(report["target_version"], expected.target_version);
    assert_eq!(report["pending"], serde_json::json!(expected.pending));
    assert_eq!(report["existing"], true);
    assert_eq!(fs::read(&ledger).unwrap(), before);
    assert!(!data_dir.join("backups").exists());
    assert!(!data_dir.join("session.key").exists());
    assert!(!data_dir.join("settlement.lock").exists());
}

#[tokio::test]
async fn published_v0101_history_opens_without_schema_or_data_changes() {
    let temporary = TempDir::new().unwrap();
    let data_dir = temporary.path().join("published-data");
    fs::create_dir(&data_dir).unwrap();
    let ledger = data_dir.join("settlement.sqlite3");
    create_published_ledger(&ledger).await;

    let before = database_snapshot(&ledger).await;
    let recorded = before
        .migrations
        .iter()
        .map(|(version, checksum, _)| (*version, checksum.clone()))
        .collect::<Vec<_>>();
    assert_eq!(recorded, published_migration_records());

    let inspection = migrations::inspect(&ledger)
        .await
        .expect("inspect published v0.10.1 ledger");
    assert_eq!(inspection.schema_version, 7);
    assert_eq!(inspection.target_version, 7);
    assert!(inspection.pending.is_empty());
    assert!(inspection.existing);

    let store = Store::open(&ledger)
        .await
        .expect("open published v0.10.1 ledger");
    store.close().await;
    assert_eq!(database_snapshot(&ledger).await, before);

    let output = run_migrate_check(&data_dir);
    assert!(
        output.status.success(),
        "migrate --check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_json_stdout(&output);
    assert_eq!(report["schema_version"], 7);
    assert_eq!(report["target_version"], 7);
    assert_eq!(report["pending"], serde_json::json!([]));
    assert_eq!(report["existing"], true);
    assert_eq!(database_snapshot(&ledger).await, before);
}

#[tokio::test]
async fn new_ledger_writes_the_published_v0101_checksum_vector() {
    let temporary = TempDir::new().unwrap();
    let ledger = temporary.path().join("settlement.sqlite3");
    let store = Store::open(&ledger).await.expect("create new ledger");
    store.close().await;

    let before = database_snapshot(&ledger).await;
    let recorded = before
        .migrations
        .iter()
        .map(|(version, checksum, _)| (*version, checksum.clone()))
        .collect::<Vec<_>>();
    assert_eq!(recorded, published_migration_records());

    let inspection = migrations::inspect(&ledger)
        .await
        .expect("inspect new ledger with published checksums");
    assert_eq!(inspection.schema_version, 7);
    assert_eq!(inspection.target_version, 7);
    assert!(inspection.pending.is_empty());

    let store = Store::open(&ledger)
        .await
        .expect("reopen new ledger with published checksum history");
    store.close().await;
    assert_eq!(database_snapshot(&ledger).await, before);
}

#[tokio::test]
async fn checksum_tampering_and_unknown_migration_version_leave_ledger_unchanged() {
    for (case, mutation) in [
        (
            "checksum",
            "UPDATE rust_schema_migrations SET checksum='tampered' WHERE version=1",
        ),
        (
            "unknown-version",
            "INSERT INTO rust_schema_migrations(version,checksum,applied_at) VALUES(99,'unknown','fixture')",
        ),
    ] {
        let temporary = TempDir::new().unwrap();
        let directory = temporary.path().join(case);
        fs::create_dir(&directory).unwrap();
        let ledger = directory.join("settlement.sqlite3");
        let store = Store::open(&ledger).await.expect("create migrated ledger");
        store.close().await;

        let mut db = connect(&ledger).await;
        sqlx::query(mutation).execute(&mut db).await.unwrap();
        db.close().await.unwrap();
        let before = fs::read(&ledger).unwrap();
        let before_cycles: i64 = {
            let mut db = connect_read_only(&ledger).await;
            let count = sqlx::query_scalar("SELECT count(*) FROM cycles")
                .fetch_one(&mut db)
                .await
                .unwrap();
            db.close().await.unwrap();
            count
        };

        let error = Store::open(&ledger)
            .await
            .err()
            .expect("incompatible schema history must refuse open");
        assert!(
            error.message.contains("不兼容"),
            "{case}: {}",
            error.message
        );
        assert_eq!(
            fs::read(&ledger).unwrap(),
            before,
            "{case} changed database bytes"
        );
        let mut db = connect_read_only(&ledger).await;
        let after_cycles: i64 = sqlx::query_scalar("SELECT count(*) FROM cycles")
            .fetch_one(&mut db)
            .await
            .unwrap();
        db.close().await.unwrap();
        assert_eq!(after_cycles, before_cycles, "{case} changed financial rows");
        assert!(
            !directory.join("backups").exists(),
            "{case} created a backup"
        );
    }
}
