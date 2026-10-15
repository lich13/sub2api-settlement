use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use settlement::{config::Config, http::App, store};
use tempfile::TempDir;

const CYCLE_START: &str = "2025-01-01T00:00:00+00:00";

fn config(directory: &Path, fixture: PathBuf) -> Config {
    Config {
        data_dir: directory.join("data"),
        source_url: String::new(),
        fixture: Some(fixture),
        origin: "https://example.invalid".to_owned(),
        verify_url: "http://127.0.0.1:1".to_owned(),
        bind: "127.0.0.1:0".parse().unwrap(),
        background: false,
        read_only: true,
        external_writes: false,
        test: true,
        revision: "test-fixture".to_owned(),
        assets: directory.join("assets"),
    }
}

fn gap_fixture() -> Value {
    let mut source: Value = serde_json::from_str(include_str!("compat/source.json")).unwrap();
    let remaining_usage = source["usage"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] != 2)
        .cloned()
        .collect::<Vec<_>>();
    source["usage"] = json!(remaining_usage);
    source["cleanups"] = json!([{
        "id": 9001,
        "filters": {
            "start_time": "2025-01-09T00:00:00+00:00",
            "end_time": "2025-01-21T00:00:00+00:00",
            "account_id": 100,
            "user_id": 3
        },
        "deleted_rows": 2,
        "finished_at": "2025-04-01T00:00:00+00:00"
    }]);
    source
}

async fn gaps(app: &App) -> Value {
    let mut db = app.service.store.read().await.unwrap();
    let gaps = store::meta(&mut db, "coverage_gaps").await.unwrap();
    db.commit().await.unwrap();
    gaps
}

async fn assert_cycle_rejected_without_writes(app: &App) {
    let admin = json!({"id": 1, "name": "Fixture Admin", "role": "admin"});
    let payload = json!({
        "account_id": 100,
        "name": "Fixture restart-gap cycle",
        "cost": "10.00",
        "creditor_id": 2,
        "start": CYCLE_START,
        "start_mode": "exact",
        "cost_kind": "exact"
    });
    let mut db = app.service.store.write().await.unwrap();
    let error = app
        .service
        .create_cycle(&mut db, &admin, &payload)
        .await
        .unwrap_err();
    assert_eq!(error.status, 422);
    assert!(
        error.message.contains("该周期存在未采集的历史记录"),
        "unexpected rejection: {}",
        error.message
    );

    let cycles = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cycles")
        .fetch_one(&mut *db)
        .await
        .unwrap();
    let audit = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit")
        .fetch_one(&mut *db)
        .await
        .unwrap();
    assert_eq!(cycles, 0);
    assert_eq!(audit, 0);
    db.rollback().await.unwrap();
}

#[tokio::test]
async fn persisted_coverage_gap_blocks_cycle_before_and_after_restart_sync() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    std::fs::write(
        &fixture,
        serde_json::to_vec(
            &serde_json::from_str::<Value>(include_str!("compat/source.json")).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let app = App::new(config(temporary.path(), fixture.clone()))
        .await
        .unwrap();
    app.service.sync(true).await.unwrap();

    std::fs::write(&fixture, serde_json::to_vec(&gap_fixture()).unwrap()).unwrap();
    app.service.sync(true).await.unwrap();
    let persisted = gaps(&app).await;
    assert_eq!(persisted.as_array().unwrap().len(), 1);
    assert_eq!(persisted[0]["root_id"], 100);

    app.service.source.close().await;
    app.service.store.close().await;

    let restarted = App::new(config(temporary.path(), fixture)).await.unwrap();
    assert_eq!(gaps(&restarted).await, persisted);
    // App::new reopens the persisted database but does not sync the source.
    // This is the first cycle validation after restart.
    assert_cycle_rejected_without_writes(&restarted).await;

    restarted.service.sync(true).await.unwrap();
    assert_eq!(gaps(&restarted).await, persisted);
    assert_cycle_rejected_without_writes(&restarted).await;

    restarted.service.source.close().await;
    restarted.service.store.close().await;
}
