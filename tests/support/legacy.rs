use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use settlement::{config::Config, http::App, store};
use tempfile::TempDir;

pub const START: &str = "2025-01-01T00:00:00+00:00";
pub const END: &str = "2025-02-01T00:00:00+00:00";
pub const NEXT_END: &str = "2025-03-01T00:00:00+00:00";

pub fn admin() -> Value {
    json!({"id":1,"name":"Fixture Admin","role":"admin"})
}

pub fn creditor() -> Value {
    json!({"id":2,"name":"Fixture Creditor","role":"user"})
}

pub fn payer() -> Value {
    json!({"id":3,"name":"Fixture Payer","role":"user"})
}

pub fn other() -> Value {
    json!({"id":4,"name":"Fixture Other","role":"user"})
}

pub fn stranger() -> Value {
    json!({"id":5,"name":"Fixture Stranger","role":"user"})
}

pub fn fixture_data() -> Value {
    serde_json::from_str(include_str!("../compat/source.json"))
        .expect("tests/compat/source.json is valid JSON")
}

pub fn config(data_dir: &Path, fixture: PathBuf) -> Config {
    Config {
        data_dir: data_dir.join("data"),
        source_url: String::new(),
        fixture: Some(fixture),
        origin: "https://example.invalid".to_owned(),
        verify_url: "http://127.0.0.1:1".to_owned(),
        bind: "127.0.0.1:0".parse().expect("loopback bind address"),
        background: false,
        read_only: false,
        external_writes: false,
        test: true,
        revision: "legacy-behavior-test".to_owned(),
        assets: data_dir.join("assets"),
    }
}

pub struct Harness {
    pub app: App,
    _temporary: TempDir,
    fixture_path: PathBuf,
}

impl Harness {
    pub async fn new() -> Self {
        Self::with_fixture(fixture_data()).await
    }

    pub async fn with_fixture(data: Value) -> Self {
        let temporary = TempDir::new().expect("temporary test directory");
        let fixture_path = temporary.path().join("source.json");
        write_fixture(&fixture_path, &data);
        let app = App::new(config(temporary.path(), fixture_path.clone()))
            .await
            .expect("isolated test app");
        app.service.sync(true).await.expect("initial fixture sync");
        Self {
            app,
            _temporary: temporary,
            fixture_path,
        }
    }

    pub fn source(&self) -> Value {
        serde_json::from_slice(
            &std::fs::read(&self.fixture_path).expect("read synthetic source fixture"),
        )
        .expect("synthetic source fixture remains valid JSON")
    }

    pub fn write_source(&self, data: &Value) {
        write_fixture(&self.fixture_path, data);
    }

    pub async fn peer(&self) -> App {
        App::new(config(self._temporary.path(), self.fixture_path.clone()))
            .await
            .expect("second isolated app over the same test database")
    }

    pub async fn rows(&self, table: &str) -> Vec<Value> {
        assert!(
            [
                "audit",
                "bill_imports",
                "batches",
                "cycles",
                "expenses",
                "netting_payments",
                "orders",
                "profiles",
                "requests",
                "settlements",
                "usage",
            ]
            .contains(&table),
            "unexpected test table {table}"
        );
        let mut db = self.app.service.store.read().await.unwrap();
        let order = if matches!(table, "profiles" | "requests") {
            "rowid"
        } else {
            "id"
        };
        let result = store::all(
            &mut db,
            &format!("SELECT * FROM {table} ORDER BY {order}"),
            &[],
        )
        .await
        .unwrap();
        db.commit().await.unwrap();
        result
    }
}

fn write_fixture(path: &Path, data: &Value) {
    std::fs::write(
        path,
        serde_json::to_vec(data).expect("serialize synthetic source fixture"),
    )
    .expect("write synthetic source fixture");
}
