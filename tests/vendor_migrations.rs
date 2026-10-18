use std::{
    fs,
    path::{Path, PathBuf},
};

use settlement::store::{Store, migrations};
use sqlx::{ConnectOptions, Connection, Row, sqlite::SqliteConnectOptions};
use tempfile::TempDir;

const LEGACY_SQL: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/ledger/legacy-written.sql"
);
const VENDOR_SCHEMA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/vendor_schema.sql");
const PUBLISHED_MIGRATION_CHECKSUMS: [&str; 7] = [
    "2f7d2bb503201536c4f0b51471b56dec24fa3e4a43eee98f777ef55e6f2ee176",
    "30d331e26fec4786252dbd5ba5353b477002a943ba971c3eb7765f2a36e46ac0",
    "ff99cd10deeb058026124228ac96d6cc333948bbeb590c09e4bfe77e33f5954d",
    "8c27e69c2df11b7cb062e12ab71ec605f2ab2cc96476fa15e01481f16b839fe2",
    "c6318f749e4276b2774c258a81dc31a3ca8f704b033d0fd822fe189ec9853d2b",
    "8c11465571dfeb251884247febafc2852b4a292364e496afe69393a9caebec54",
    "f6a9788b9fb5818fef31c4866aa8716d43f9d2ecd637c09129658a45c5efb53b",
];
const CURRENT_MIGRATION_VERSION: i64 = 9;

#[derive(Debug, PartialEq)]
struct EstimatedCycle {
    cost_fen: i64,
    cost_kind: String,
    cost_source: String,
    bill_import_id: Option<i64>,
    bill_end: Option<String>,
    bill_digest: Option<String>,
    bill_difference_confirmed: i64,
    bill_allocation_mode: String,
    billable_cost_fen: Option<i64>,
}

#[derive(Debug, PartialEq)]
struct SettlementSnapshot {
    id: i64,
    cycle_id: Option<i64>,
    expense_id: Option<i64>,
    snapshot: String,
    digest: String,
}

#[derive(Debug, PartialEq)]
struct OrderAmount {
    id: i64,
    cycle_id: Option<i64>,
    expense_id: Option<i64>,
    amount_fen: i64,
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

async fn import_legacy_fixture(path: &Path) {
    let sql = fs::read_to_string(LEGACY_SQL).expect("read legacy ledger fixture");
    let mut db = connect(path).await;
    sqlx::raw_sql(&sql)
        .execute(&mut db)
        .await
        .expect("import legacy ledger fixture");
    db.close().await.expect("close imported legacy ledger");
}

async fn create_published_v7_ledger(path: &Path) {
    import_legacy_fixture(path).await;
    let mut db = connect(path).await;
    sqlx::query(
        "CREATE TABLE rust_schema_migrations(version INTEGER PRIMARY KEY,checksum TEXT NOT NULL,applied_at TEXT NOT NULL)",
    )
    .execute(&mut db)
    .await
    .expect("create published v7 migration history");
    for (index, checksum) in PUBLISHED_MIGRATION_CHECKSUMS.iter().enumerate() {
        sqlx::query(
            "INSERT INTO rust_schema_migrations(version,checksum,applied_at) VALUES(?,?,?)",
        )
        .bind(index as i64 + 1)
        .bind(*checksum)
        .bind("published-v0.10.1")
        .execute(&mut db)
        .await
        .expect("insert published migration record");
    }
    db.close().await.expect("close published v7 ledger");
}

async fn create_published_v8_ledger(path: &Path) {
    create_published_v7_ledger(path).await;
    let mut db = connect(path).await;
    let vendor_schema = fs::read_to_string(VENDOR_SCHEMA).expect("read vendor schema");
    sqlx::raw_sql(&vendor_schema)
        .execute(&mut db)
        .await
        .expect("apply published vendor schema");
    sqlx::raw_sql(
        "ALTER TABLE profiles ADD COLUMN vendor_id INTEGER;
         ALTER TABLE cycles ADD COLUMN vendor_id INTEGER;
         ALTER TABLE cycles ADD COLUMN vendor_period_id INTEGER;
         CREATE INDEX profile_vendor ON profiles(vendor_id);
         CREATE INDEX cycle_vendor_period ON cycles(vendor_period_id,id);",
    )
    .execute(&mut db)
    .await
    .expect("apply published vendor columns");
    let checksum = settlement::json::hash(vendor_schema.as_bytes());
    sqlx::query("INSERT INTO rust_schema_migrations(version,checksum,applied_at) VALUES(8,?,?)")
        .bind(checksum)
        .bind("published-v0.11.1")
        .execute(&mut db)
        .await
        .expect("record published v8 migration");
    db.close().await.expect("close published v8 ledger");
}

async fn add_estimated_cycle(path: &Path) {
    let mut db = connect(path).await;
    sqlx::query(
        "INSERT INTO cycles(
            id,account_id,name,cost_fen,creditor_id,start,end,status,version,settlement_id,
            start_mode,start_lower,start_upper,start_confirmed,start_confirmed_by,
            start_confirmed_at,start_source_usage_id,renewed_from_cycle_id,start_locked,
            start_anchor,cost_kind,cost_source,bill_import_id,bill_end,bill_digest,
            bill_difference_confirmed,bill_allocation_mode,billable_cost_fen
        ) VALUES(
            99,100,'Fixture estimated preserved',9876,2,
            '2025-03-01T00:00:00.000000+00:00',NULL,'open',7,NULL,
            'exact','2025-03-01T00:00:00.000000+00:00',NULL,1,NULL,
            NULL,NULL,NULL,0,NULL,'estimated','legacy_fixture',NULL,NULL,NULL,
            0,'proportional',NULL
        )",
    )
    .execute(&mut db)
    .await
    .expect("add estimated legacy cycle");
    db.close().await.expect("close estimated cycle fixture");
}

async fn estimated_cycle(path: &Path) -> EstimatedCycle {
    let mut db = connect(path).await;
    let row = sqlx::query(
        "SELECT cost_fen,cost_kind,cost_source,bill_import_id,bill_end,bill_digest,
                bill_difference_confirmed,bill_allocation_mode,billable_cost_fen
         FROM cycles WHERE id=99",
    )
    .fetch_one(&mut db)
    .await
    .expect("read estimated cycle");
    let result = EstimatedCycle {
        cost_fen: row.try_get("cost_fen").unwrap(),
        cost_kind: row.try_get("cost_kind").unwrap(),
        cost_source: row.try_get("cost_source").unwrap(),
        bill_import_id: row.try_get("bill_import_id").unwrap(),
        bill_end: row.try_get("bill_end").unwrap(),
        bill_digest: row.try_get("bill_digest").unwrap(),
        bill_difference_confirmed: row.try_get("bill_difference_confirmed").unwrap(),
        bill_allocation_mode: row.try_get("bill_allocation_mode").unwrap(),
        billable_cost_fen: row.try_get("billable_cost_fen").unwrap(),
    };
    db.close().await.expect("close estimated cycle read");
    result
}

async fn settlement_snapshots(path: &Path) -> Vec<SettlementSnapshot> {
    let mut db = connect(path).await;
    let rows =
        sqlx::query("SELECT id,cycle_id,expense_id,snapshot,digest FROM settlements ORDER BY id")
            .fetch_all(&mut db)
            .await
            .expect("read settlement snapshots");
    let result = rows
        .iter()
        .map(|row| SettlementSnapshot {
            id: row.try_get("id").unwrap(),
            cycle_id: row.try_get("cycle_id").unwrap(),
            expense_id: row.try_get("expense_id").unwrap(),
            snapshot: row.try_get("snapshot").unwrap(),
            digest: row.try_get("digest").unwrap(),
        })
        .collect();
    db.close().await.expect("close settlement snapshot read");
    result
}

async fn order_amounts(path: &Path) -> Vec<OrderAmount> {
    let mut db = connect(path).await;
    let rows = sqlx::query("SELECT id,cycle_id,expense_id,amount_fen FROM orders ORDER BY id")
        .fetch_all(&mut db)
        .await
        .expect("read order amounts");
    let result = rows
        .iter()
        .map(|row| OrderAmount {
            id: row.try_get("id").unwrap(),
            cycle_id: row.try_get("cycle_id").unwrap(),
            expense_id: row.try_get("expense_id").unwrap(),
            amount_fen: row.try_get("amount_fen").unwrap(),
        })
        .collect();
    db.close().await.expect("close order amount read");
    result
}

async fn table_names(path: &Path) -> Vec<String> {
    let mut db = connect(path).await;
    let names = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(&mut db)
    .await
    .expect("read table names");
    db.close().await.expect("close table name read");
    names
}

async fn columns(path: &Path, table: &str) -> Vec<String> {
    let mut db = connect(path).await;
    let names = sqlx::query_scalar::<_, String>(&format!(
        "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
    ))
    .fetch_all(&mut db)
    .await
    .expect("read table columns");
    db.close().await.expect("close column read");
    names
}

async fn foreign_key_check(path: &Path) -> usize {
    let mut db = connect(path).await;
    let count = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut db)
        .await
        .expect("run foreign key check")
        .len();
    db.close().await.expect("close foreign key check");
    count
}

async fn migration_pairs(path: &Path) -> Vec<(i64, String)> {
    let mut db = connect(path).await;
    let rows = sqlx::query("SELECT version,checksum FROM rust_schema_migrations ORDER BY version")
        .fetch_all(&mut db)
        .await
        .expect("read migration records");
    let result = rows
        .iter()
        .map(|row| {
            (
                row.try_get("version").unwrap(),
                row.try_get("checksum").unwrap(),
            )
        })
        .collect();
    db.close().await.expect("close migration read");
    result
}

fn backup_files(directory: &Path) -> Vec<PathBuf> {
    if !directory.is_dir() {
        return Vec::new();
    }
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

#[tokio::test]
async fn fresh_ledger_applies_vendor_schema_and_clean_foreign_keys() {
    let temporary = TempDir::new().unwrap();
    let ledger = temporary.path().join("settlement.sqlite3");

    let store = Store::open(&ledger).await.expect("create fresh ledger");
    store.close().await;

    let names = table_names(&ledger).await;
    for table in [
        "vendors",
        "vendor_periods",
        "vendor_bill_imports",
        "vendor_bill_previews",
    ] {
        assert!(names.iter().any(|name| name == table), "missing {table}");
    }
    let profile_columns = columns(&ledger, "profiles").await;
    assert!(profile_columns.iter().any(|column| column == "vendor_id"));
    let cycle_columns = columns(&ledger, "cycles").await;
    for column in ["vendor_id", "vendor_period_id"] {
        assert!(cycle_columns.iter().any(|name| name == column));
    }
    assert_eq!(foreign_key_check(&ledger).await, 0);
    let migrations = migration_pairs(&ledger).await;
    assert_eq!(migrations.len(), CURRENT_MIGRATION_VERSION as usize);
    assert_eq!(
        &migrations[..PUBLISHED_MIGRATION_CHECKSUMS.len()],
        &PUBLISHED_MIGRATION_CHECKSUMS
            .iter()
            .enumerate()
            .map(|(index, checksum)| (index as i64 + 1, (*checksum).to_owned()))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn published_v7_upgrade_preserves_estimated_rows_issued_snapshots_and_amounts() {
    let temporary = TempDir::new().unwrap();
    let ledger = temporary.path().join("settlement.sqlite3");
    create_published_v7_ledger(&ledger).await;
    add_estimated_cycle(&ledger).await;

    let before_estimated = estimated_cycle(&ledger).await;
    let before_settlements = settlement_snapshots(&ledger).await;
    let before_orders = order_amounts(&ledger).await;
    let inspection = migrations::inspect(&ledger)
        .await
        .expect("inspect v7 ledger");
    assert_eq!(inspection.schema_version, 7);
    assert_eq!(inspection.target_version, CURRENT_MIGRATION_VERSION);
    assert_eq!(inspection.pending, vec![8, 9]);

    let store = Store::open(&ledger).await.expect("upgrade v7 ledger");
    store.close().await;

    assert_eq!(estimated_cycle(&ledger).await, before_estimated);
    assert_eq!(settlement_snapshots(&ledger).await, before_settlements);
    assert_eq!(order_amounts(&ledger).await, before_orders);
    assert_eq!(foreign_key_check(&ledger).await, 0);
    let migrations = migration_pairs(&ledger).await;
    assert_eq!(migrations.len(), CURRENT_MIGRATION_VERSION as usize);
    assert_eq!(
        &migrations[..PUBLISHED_MIGRATION_CHECKSUMS.len()],
        PUBLISHED_MIGRATION_CHECKSUMS
            .iter()
            .enumerate()
            .map(|(index, checksum)| (index as i64 + 1, (*checksum).to_owned()))
            .collect::<Vec<_>>()
    );

    let names = table_names(&ledger).await;
    for table in [
        "vendors",
        "vendor_periods",
        "vendor_bill_imports",
        "vendor_bill_previews",
    ] {
        assert!(names.iter().any(|name| name == table), "missing {table}");
    }
    let mut db = connect(&ledger).await;
    let non_null_profiles: i64 =
        sqlx::query_scalar("SELECT count(*) FROM profiles WHERE vendor_id IS NOT NULL")
            .fetch_one(&mut db)
            .await
            .unwrap();
    let non_null_cycles: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cycles WHERE vendor_id IS NOT NULL OR vendor_period_id IS NOT NULL",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    db.close().await.unwrap();
    assert_eq!(non_null_profiles, 0);
    assert_eq!(non_null_cycles, 0);
}

#[tokio::test]
async fn published_v7_upgrade_restart_is_idempotent_and_does_not_add_backups() {
    let temporary = TempDir::new().unwrap();
    let data_dir = temporary.path().join("data");
    fs::create_dir(&data_dir).unwrap();
    let ledger = data_dir.join("settlement.sqlite3");
    create_published_v7_ledger(&ledger).await;

    let store = Store::open(&ledger).await.expect("first v7 upgrade");
    store.close().await;
    let backups = backup_files(&data_dir.join("backups"));
    assert_eq!(backups.len(), 1);
    assert_eq!(foreign_key_check(&ledger).await, 0);

    let store = Store::open(&ledger).await.expect("restart upgraded ledger");
    store.close().await;
    assert_eq!(backup_files(&data_dir.join("backups")), backups);
    assert_eq!(
        migration_pairs(&ledger).await.len(),
        CURRENT_MIGRATION_VERSION as usize
    );
    assert_eq!(foreign_key_check(&ledger).await, 0);
}

#[tokio::test]
async fn published_v8_upgrade_to_manual_billing_preserves_history() {
    let temporary = TempDir::new().unwrap();
    let ledger = temporary.path().join("settlement.sqlite3");
    create_published_v8_ledger(&ledger).await;
    add_estimated_cycle(&ledger).await;

    let before_estimated = estimated_cycle(&ledger).await;
    let before_settlements = settlement_snapshots(&ledger).await;
    let before_orders = order_amounts(&ledger).await;
    let inspection = migrations::inspect(&ledger)
        .await
        .expect("inspect v8 ledger");
    assert_eq!(inspection.schema_version, 8);
    assert_eq!(inspection.target_version, 9);
    assert_eq!(inspection.pending, vec![9]);

    let store = Store::open(&ledger).await.expect("upgrade v8 ledger");
    store.close().await;

    assert_eq!(estimated_cycle(&ledger).await, before_estimated);
    assert_eq!(settlement_snapshots(&ledger).await, before_settlements);
    assert_eq!(order_amounts(&ledger).await, before_orders);
    assert_eq!(foreign_key_check(&ledger).await, 0);
    assert!(
        table_names(&ledger)
            .await
            .iter()
            .any(|name| name == "manual_bill_bases")
    );
    for table in ["cycles", "vendor_periods"] {
        assert!(
            columns(&ledger, table)
                .await
                .iter()
                .any(|name| name == "manual_basis_id")
        );
    }
    let migrations = migration_pairs(&ledger).await;
    assert_eq!(migrations.len(), 9);
    assert_eq!(migrations[7].0, 8);
    assert_eq!(migrations[8].0, 9);
}
