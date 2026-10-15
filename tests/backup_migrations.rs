#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use settlement::{backup, store::Store};
use sqlx::{ConnectOptions, Connection, Row, sqlite::SqliteConnectOptions};
use tempfile::TempDir;

const LEGACY_SQL: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/ledger/legacy-written.sql"
);

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
    assert_eq!(version, 3);
    migrated.close().await.unwrap();

    let store = Store::open(&ledger).await.expect("reopen migrated ledger");
    store.close().await;
    assert_eq!(backup_files(&backup_directory), backups);
}

#[test]
fn migrate_check_on_missing_data_directory_is_read_only_and_creates_nothing() {
    let temporary = TempDir::new().unwrap();
    let data_dir = temporary.path().join("not-created").join("data");

    let output = run_migrate_check(&data_dir);
    assert!(
        output.status.success(),
        "migrate --check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_json_stdout(&output);
    assert_eq!(report["schema_version"], 0);
    assert_eq!(report["target_version"], 3);
    assert_eq!(report["pending"], serde_json::json!([1, 2, 3]));
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

    let output = run_migrate_check(&data_dir);
    assert!(
        output.status.success(),
        "migrate --check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_json_stdout(&output);
    assert_eq!(report["schema_version"], 0);
    assert_eq!(report["target_version"], 3);
    assert_eq!(report["pending"], serde_json::json!([1, 2, 3]));
    assert_eq!(report["existing"], true);
    assert_eq!(fs::read(&ledger).unwrap(), before);
    assert!(!data_dir.join("backups").exists());
    assert!(!data_dir.join("session.key").exists());
    assert!(!data_dir.join("settlement.lock").exists());
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
