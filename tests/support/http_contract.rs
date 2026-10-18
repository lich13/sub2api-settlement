#![allow(dead_code)]

#[path = "presentation_contract.rs"]
mod presentation_contract;
#[path = "http_contract_races.rs"]
mod races;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    net::{SocketAddr, TcpListener as StdTcpListener},
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path as AxumPath, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use chrono::{DateTime, NaiveDateTime, SecondsFormat};
use fernet::Fernet;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Column, Connection, Row, TypeInfo, ValueRef,
    sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode},
};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    process::{Child as TokioChild, Command as TokioCommand},
    sync::Mutex,
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const FIXTURE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const WORKFLOW_SCHEMA: &str = "settlement-http-workflow/v1";
const TABLES: &[&str] = &[
    "meta",
    "users",
    "accounts",
    "profiles",
    "usage",
    "cycles",
    "previews",
    "settlements",
    "settlement_usage",
    "batches",
    "orders",
    "notifications",
    "audit",
    "requests",
    "sessions",
    "usage_details",
    "expenses",
    "bill_imports",
    "bill_previews",
    "account_deletions",
    "netting_payments",
];
const JSON_COLUMNS: &[&str] = &[
    "payload", "snapshot", "members", "rows", "response", "value",
];
const GENERATED_TIMES: &[&str] = &[
    "created_at",
    "updated_at",
    "paid_at",
    "submitted_at",
    "last_sync",
    "account_revision",
    "revision",
    "checked_at",
    "start_confirmed_at",
];
const SECURITY_HEADERS: &[&str] = &[
    "cache-control",
    "referrer-policy",
    "x-content-type-options",
    "x-frame-options",
    "content-security-policy",
];
const HARDENED_ERROR_HEADERS: &[(&str, &str)] = &[
    ("cache-control", "no-store"),
    ("referrer-policy", "no-referrer"),
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "SAMEORIGIN"),
    (
        "content-security-policy",
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'self'; base-uri 'self'; form-action 'self'",
    ),
];

type TestResult<T> = std::result::Result<T, String>;

pub async fn run_workflow(name: &str) -> TestResult<()> {
    let mut harness = Harness::new_with_contract(name, false).await?;
    harness.run().await
}

/// Run a frozen v0.8 workflow under the explicit v0.11 cost contract.
///
/// The fixture bytes remain the historical contract. The runner records the
/// two deliberate differences: a new estimated-cost write is rejected, then
/// the historical row is inserted as a compatibility fixture so the rest of
/// the billing and settlement workflow stays covered; renewal costs now use
/// the exact default.
pub async fn run_workflow_v011(name: &str) -> TestResult<()> {
    let mut harness = Harness::new_with_contract(name, true).await?;
    harness.run().await
}

#[tokio::test]
async fn deletion_jobs_waits_for_delayed_delete() {
    let mut harness = Harness::new("deletion-jobs").await.unwrap();
    harness.upstream.state.inner.lock().await.delete_delay = Duration::from_millis(250);
    harness
        .run()
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        harness.pending_upstream_observations > 0,
        "the observation barrier must run while the successful DELETE is still delayed"
    );

    let observations =
        std::mem::take(&mut harness.upstream.state.inner.lock().await.delayed_deletes);
    assert_eq!(
        observations.len(),
        1,
        "the successful DELETE must be delayed once"
    );
    let observation = &observations[0];
    let before = observation.source_before.as_ref().unwrap();
    let after = observation.source_after.as_ref().unwrap();
    assert!(
        before["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|account| { account["id"].as_i64() == Some(observation.account_id) }),
        "the account must still exist when the delayed DELETE begins"
    );
    assert_eq!(
        before, after,
        "the replay must not apply the captured deletion while DELETE is delayed"
    );
}

struct Harness {
    name: String,
    v011_cost_contract: bool,
    root: PathBuf,
    workflow: Value,
    replay_overlay: Value,
    _temporary: TempDir,
    data_dir: PathBuf,
    source_path: PathBuf,
    session_key: Option<String>,
    session_vectors: Vec<Value>,
    clock_mode: String,
    started_at_epoch: f64,
    origin: String,
    bind: SocketAddr,
    service: Option<TokioChild>,
    service_log: PathBuf,
    service_generation: usize,
    client: reqwest::Client,
    aliases: AliasRegistry,
    presentation: presentation_contract::PresentationContract,
    actor_cookies: HashMap<i64, String>,
    csrf_by_sso: HashMap<String, String>,
    signatures: HashMap<(i64, String), SignatureInput>,
    upstream_call_cursor: usize,
    pending_upstream_observations: usize,
    race_winners: BTreeMap<String, String>,
    upstream: UpstreamServer,
}

impl Harness {
    async fn new(name: &str) -> TestResult<Self> {
        Self::new_with_contract(name, false).await
    }

    async fn new_with_contract(name: &str, v011_cost_contract: bool) -> TestResult<Self> {
        let root = PathBuf::from(FIXTURE_ROOT);
        let workflow_path = root.join("http/workflows").join(format!("{name}.json"));
        let workflow: Value = read_json(&workflow_path)?;
        if workflow["schema_version"] != WORKFLOW_SCHEMA {
            return Err(format!(
                "{name}: unsupported fixture schema {:?}",
                workflow["schema_version"]
            ));
        }
        if workflow["workflow"] != name {
            return Err(format!(
                "{name}: fixture workflow name does not match filename"
            ));
        }
        let replay_overlay = read_json(&root.join("http/replay-provenance.json"))?;
        if replay_overlay["schema_version"] != "settlement-static-replay-overlay/v1" {
            return Err(format!("{name}: unsupported static replay overlay"));
        }

        let temporary = TempDir::new().map_err(display_error)?;
        let data_dir = temporary.path().join("data");
        fs::create_dir_all(&data_dir).map_err(display_error)?;
        let source_path = data_dir.join("source.json");

        let seed = &workflow["seed"];
        let source_fixture = fixture_path(&root, seed["upstream_json"].as_str(), name)?;
        fs::copy(&source_fixture, &source_path).map_err(display_error)?;

        let ledger_fixture = fixture_path(&root, seed["ledger_sql"].as_str(), name)?;
        seed_database(&data_dir.join("settlement.sqlite3"), &ledger_fixture).await?;

        let session_vectors = load_session_vectors(&root, name)?;
        let session_key = write_session_key(&data_dir, &session_vectors)?;
        let clock_mode = workflow["clock"]["mode"]
            .as_str()
            .ok_or_else(|| format!("{name}: clock mode missing"))?
            .to_owned();
        if !["fixed", "runtime"].contains(&clock_mode.as_str()) {
            return Err(format!("{name}: unsupported clock mode {clock_mode:?}"));
        }
        let started_at_epoch = system_epoch();

        let port = free_port()?;
        let bind = SocketAddr::from(([127, 0, 0, 1], port));
        let origin = format!("http://{bind}");
        let upstream = UpstreamServer::start(source_path.clone()).await?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(display_error)?;

        let mut aliases = AliasRegistry::default();
        aliases.bind("<origin>", origin.clone())?;
        let mut presentation = presentation_contract::PresentationContract::default();
        presentation.set_source_state(&read_json(&source_fixture)?);
        Ok(Self {
            name: name.to_owned(),
            v011_cost_contract,
            root,
            workflow,
            replay_overlay,
            _temporary: temporary,
            data_dir,
            source_path,
            session_key,
            session_vectors,
            clock_mode,
            started_at_epoch,
            origin,
            bind,
            service: None,
            service_log: PathBuf::new(),
            service_generation: 0,
            client,
            aliases,
            presentation,
            actor_cookies: HashMap::new(),
            csrf_by_sso: HashMap::new(),
            signatures: HashMap::new(),
            upstream_call_cursor: 0,
            pending_upstream_observations: 0,
            race_winners: BTreeMap::new(),
            upstream,
        })
    }

    async fn run(&mut self) -> TestResult<()> {
        self.start_service().await?;
        let steps = self.workflow["steps"]
            .as_array()
            .ok_or_else(|| format!("{}: fixture steps is not an array", self.name))?
            .clone();
        for (step_index, step) in steps.iter().enumerate() {
            let label = step["label"].as_str().unwrap_or("unlabelled-step");
            match step["op"].as_str().unwrap_or("") {
                "http" => self.run_http_step(step).await?,
                "sso" => self.run_sso_step(step).await?,
                "db_snapshot" => self.run_db_snapshot(step).await?,
                "upstream_state" => self.apply_upstream_state(step_index, step).await?,
                "restart" => self.restart_service().await?,
                "sql" => self.run_sql_step(step).await?,
                "race" => self.run_race_step(step).await?,
                "conditional" => self.run_conditional_step(step).await?,
                other => {
                    return Err(format!(
                        "{} / {label}: unsupported fixture operation {other:?}",
                        self.name
                    ));
                }
            }
        }
        self.stop_service().await?;
        let calls = self.upstream.calls().await;
        if calls.as_array().map_or(0, Vec::len) != self.upstream_call_cursor {
            return Err(format!(
                "{}: unconsumed upstream calls after final step: {}",
                self.name,
                Value::Array(
                    calls
                        .as_array()
                        .unwrap()
                        .iter()
                        .skip(self.upstream_call_cursor)
                        .cloned()
                        .collect()
                )
            ));
        }
        self.upstream.stop().await?;
        Ok(())
    }

    async fn start_service(&mut self) -> TestResult<()> {
        self.service_generation += 1;
        let log_path = self
            ._temporary
            .path()
            .join(format!("settlement-{}.log", self.service_generation));
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(display_error)?;
        let stdout = log.try_clone().map_err(display_error)?;
        let clock = self.workflow["clock"]["value"].as_str();
        if self.clock_mode == "fixed" && clock.is_none() {
            return Err(format!("{}: fixed clock value missing", self.name));
        }

        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend/dist");
        let mut command = TokioCommand::new(env!("CARGO_BIN_EXE_settlement"));
        command
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("SETTLEMENT_ENV", "test")
            .env_remove("SETTLEMENT_TEST_NOW")
            .env("SETTLEMENT_BACKGROUND", "false")
            .env("SETTLEMENT_READ_ONLY", "false")
            .env("SETTLEMENT_EXTERNAL_WRITES", "true")
            .env("DATA_DIR", &self.data_dir)
            .env("SOURCE_FIXTURE", &self.source_path)
            .env("SOURCE_DATABASE_URL", "")
            .env("PUBLIC_ORIGIN", &self.origin)
            .env("SUB2API_VERIFY_URL", self.upstream.origin())
            .env("SETTLEMENT_BIND", self.bind.to_string())
            .env("SETTLEMENT_ASSETS", assets)
            .env("APP_REVISION", "static-http-contract")
            .env("RUST_LOG", "error")
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(log))
            .kill_on_drop(true);
        if let Some(clock) = clock.filter(|_| self.clock_mode == "fixed") {
            command.env("SETTLEMENT_TEST_NOW", clock);
        }
        let child = command.spawn().map_err(|error| {
            format!(
                "{}: could not start {}: {error}",
                self.name,
                env!("CARGO_BIN_EXE_settlement")
            )
        })?;
        self.service = Some(child);
        self.service_log = log_path;
        self.wait_ready().await
    }

    async fn wait_ready(&mut self) -> TestResult<()> {
        let health_url = format!("{}/settlement/healthz", self.origin);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(child) = self.service.as_mut()
                && let Some(status) = child.try_wait().map_err(display_error)?
            {
                return Err(format!(
                    "{}: settlement exited before health check ({status}); log: {}",
                    self.name,
                    read_log_tail(&self.service_log)
                ));
            }
            if let Ok(response) = self.client.get(&health_url).send().await
                && response.status() == StatusCode::OK
            {
                let body = response.json::<Value>().await.map_err(display_error)?;
                if body["status"] == "ok" {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "{}: settlement did not become healthy; log: {}",
                    self.name,
                    read_log_tail(&self.service_log)
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn stop_service(&mut self) -> TestResult<()> {
        let Some(mut child) = self.service.take() else {
            return Ok(());
        };
        if child.try_wait().map_err(display_error)?.is_none()
            && let Some(pid) = child.id()
        {
            #[cfg(unix)]
            {
                let signal = StdCommand::new("kill")
                    .arg("-TERM")
                    .arg(pid.to_string())
                    .status();
                if signal.is_err() {
                    let _ = child.start_kill();
                }
            }
            #[cfg(not(unix))]
            {
                let _ = child.start_kill();
            }
        }
        let status = match timeout(Duration::from_secs(10), child.wait()).await {
            Ok(result) => result.map_err(display_error)?,
            Err(_) => {
                let _ = child.start_kill();
                child.wait().await.map_err(display_error)?
            }
        };
        #[cfg(unix)]
        let intentional_signal = {
            use std::os::unix::process::ExitStatusExt;
            status.signal() == Some(15)
        };
        #[cfg(not(unix))]
        let intentional_signal = false;
        if !status.success() && !intentional_signal {
            return Err(format!(
                "{}: settlement stopped with {status}; log: {}",
                self.name,
                read_log_tail(&self.service_log)
            ));
        }
        Ok(())
    }

    async fn restart_service(&mut self) -> TestResult<()> {
        self.stop_service().await?;
        self.start_service().await
    }

    async fn apply_upstream_state(&mut self, step_index: usize, step: &Value) -> TestResult<()> {
        let label = step["label"].as_str().unwrap_or("upstream-state");
        let state = step
            .get("state")
            .ok_or_else(|| format!("{} / {label}: upstream state missing", self.name))?;
        let observations = self.replay_overlay["upstream_state_observations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|observation| {
                observation["workflow"] == self.name
                    && observation["step_index"].as_u64() == Some(step_index as u64)
            })
            .collect::<Vec<_>>();
        if observations.len() > 1 {
            return Err(format!(
                "{} / {label}: duplicate upstream observation overlays at step {step_index}",
                self.name
            ));
        }
        if let Some(observation) = observations.first() {
            let preceding = step_index
                .checked_sub(1)
                .and_then(|index| self.workflow["steps"].get(index))
                .ok_or("upstream observation has no preceding step")?;
            if observation["preceding_step_index"].as_u64() != Some((step_index - 1) as u64)
                || observation["preceding_label"] != preceding["label"]
                || observation["operation"] != step["op"]
                || observation["state_sha256"] != sha256_hex(canonical_json(state).as_bytes())
                || observation["effect_call"]["method"] != "DELETE"
                || observation["effect_call"]["status"] != 200
            {
                return Err(format!(
                    "{} / {label}: upstream observation overlay does not match frozen step {step_index}",
                    self.name
                ));
            }
            let state = self.aliases.expand_value(state)?;
            self.presentation.set_source_state(&state);
            let effect_call = self.aliases.expand_value(&observation["effect_call"])?;
            return self
                .wait_for_upstream_observation(step_index, &state, &effect_call)
                .await;
        }
        let state = self.aliases.expand_value(state)?;
        self.presentation.set_source_state(&state);
        self.upstream.set_state(&state).await?;
        Ok(())
    }

    async fn wait_for_upstream_observation(
        &mut self,
        step_index: usize,
        expected_state: &Value,
        effect_call: &Value,
    ) -> TestResult<()> {
        // note_upstream captures this state after the old mock DELETE mutates source.
        // Leave calls unconsumed: the next HTTP step compares the entire exact sequence.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let calls = self.upstream.calls().await;
            let effect_observed = calls
                .as_array()
                .unwrap()
                .iter()
                .skip(self.upstream_call_cursor)
                .any(|call| call == effect_call);
            let state = self.upstream.current_state().await?;
            if effect_observed && state == *expected_state {
                return Ok(());
            }
            if effect_observed {
                self.pending_upstream_observations += 1;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "{} / upstream-state step {step_index}: timed out awaiting DELETE observation; \
                     expected effect {effect_call}, observed={effect_observed}; {}",
                    self.name,
                    json_difference("upstream state", expected_state, &state)
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn run_sql_step(&self, step: &Value) -> TestResult<()> {
        let label = step["label"].as_str().unwrap_or("sql-step");
        if step["boundary"] != "commit" {
            return Err(format!(
                "{} / {label}: unsupported SQL boundary {:?}",
                self.name, step["boundary"]
            ));
        }
        let statements = step["statements"]
            .as_array()
            .ok_or_else(|| format!("{} / {label}: SQL statements missing", self.name))?;
        let path = self.data_dir.join("settlement.sqlite3");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let mut connection = SqliteConnection::connect_with(&options)
            .await
            .map_err(display_error)?;
        let explicit_begin = statements.iter().any(|statement| {
            statement["sql"]
                .as_str()
                .is_some_and(|sql| sql.trim_start().to_ascii_uppercase().starts_with("BEGIN"))
        });
        if !explicit_begin {
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut connection)
                .await
                .map_err(display_error)?;
        }
        let recreates_legacy_schema = self.name == "old-schema-migration"
            && ["cycles_compat_legacy", "orders_compat_legacy"]
                .iter()
                .all(|table| {
                    statements.iter().any(|statement| {
                        statement["sql"].as_str().is_some_and(|sql| {
                            sql.to_ascii_lowercase()
                                .contains(&format!("create table {table}"))
                        })
                    })
                });
        for (index, statement) in statements.iter().enumerate() {
            let sql = statement["sql"].as_str().ok_or_else(|| {
                format!("{} / {label}: statement {index} has no SQL text", self.name)
            })?;
            let params = statement["params"].as_array().ok_or_else(|| {
                format!(
                    "{} / {label}: statement {index} has no parameters",
                    self.name
                )
            })?;
            let params = params
                .iter()
                .map(|param| self.aliases.expand_value(param))
                .collect::<TestResult<Vec<_>>>()?;
            let mut query = sqlx::query(sql);
            for param in &params {
                query = match param {
                    Value::Null => query.bind(Option::<String>::None),
                    Value::Bool(value) => query.bind(i64::from(*value)),
                    Value::Number(value) => {
                        if let Some(value) = value.as_i64() {
                            query.bind(value)
                        } else if let Some(value) = value.as_u64() {
                            let value = i64::try_from(value).map_err(display_error)?;
                            query.bind(value)
                        } else if let Some(value) = value.as_f64() {
                            query.bind(value)
                        } else {
                            return Err(format!(
                                "{} / {label}: unsupported numeric SQL parameter",
                                self.name
                            ));
                        }
                    }
                    Value::String(value) => query.bind(value.clone()),
                    Value::Array(_) | Value::Object(_) => {
                        query.bind(serde_json::to_string(param).map_err(display_error)?)
                    }
                };
            }
            query
                .execute(&mut connection)
                .await
                .map_err(|error| format!("{} / {label} statement {index}: {error}", self.name))?;
        }
        if recreates_legacy_schema {
            // This workflow reconstructs a pre-migration database from the current test schema.
            sqlx::query("DROP TABLE IF EXISTS rust_schema_migrations")
                .execute(&mut connection)
                .await
                .map_err(display_error)?;
        }
        sqlx::query("COMMIT")
            .execute(&mut connection)
            .await
            .map_err(display_error)?;
        connection.close().await.map_err(display_error)?;
        Ok(())
    }

    async fn run_http_step(&mut self, step: &Value) -> TestResult<()> {
        let request = self.replay_request(step)?;
        self.wait_for_deletion_http_state(step).await?;
        let source_state = self.upstream.current_state().await?;
        self.presentation.set_source_state(&source_state);
        let prior_settlement_ids =
            if request["method"] == "POST" && request["path"] == "/settlement/api/v1/settlements" {
                self.current_settlement_ids().await?
            } else {
                HashSet::new()
            };
        let response = self.send_request(&request, false).await?;
        self.bind_http_aliases(step, &response.body).await?;
        self.bind_clock_aliases_from_response(step, &response.body)?;
        self.bind_runtime_usage_digests(step, &response.body)
            .await?;
        self.bind_netting_balance_digest(step, &response.body)
            .await?;
        self.presentation.record_preview(step)?;
        self.presentation.record_settlement(
            step,
            &request,
            &response.body,
            response.status,
            &prior_settlement_ids,
        )?;
        if request["method"] == "POST"
            && request["path"] == "/settlement/api/v1/settlements"
            && response.status == 200
        {
            let order_rows = self.current_order_settlement_ids().await?;
            self.presentation.record_order_ids(&order_rows);
        }
        if request["method"] == "GET"
            && request["path"]
                .as_str()
                .is_some_and(|path| path.starts_with("/settlement/api/v1/cycles/"))
        {
            self.presentation
                .set_settled_cycle_ids(self.current_settled_cycle_ids().await?);
        }
        self.compare_response(step, &response, false)?;
        if self.v011_cost_contract
            && response.status == StatusCode::UNPROCESSABLE_ENTITY.as_u16()
            && self.is_v011_estimated_create(step)
        {
            self.seed_v011_legacy_estimated_cycle(&request).await?;
        }
        self.record_signature(&request, &response)?;
        self.compare_upstream_calls(step).await?;
        Ok(())
    }

    fn is_v011_estimated_create(&self, step: &Value) -> bool {
        if !self.v011_cost_contract || step["request"]["method"] != "POST" {
            return false;
        }
        let label = step["label"].as_str().unwrap_or_default();
        let workflow = self.name.as_str();
        matches!(
            (workflow, label),
            ("starts-costs", "exact")
                | ("billing-proportional", "bill-cycle")
                | ("billing-matched-only", "bill-cycle")
        ) && step["request"]["json"]["cost_kind"] == "estimated"
    }

    async fn seed_v011_legacy_estimated_cycle(&mut self, request: &Value) -> TestResult<()> {
        let payload = request["json"].as_object().ok_or_else(|| {
            format!(
                "{}: estimated compatibility request is not an object",
                self.name
            )
        })?;
        let account_id = payload["account_id"]
            .as_i64()
            .ok_or_else(|| format!("{}: estimated compatibility account is missing", self.name))?;
        let name = payload["name"]
            .as_str()
            .ok_or_else(|| format!("{}: estimated compatibility name is missing", self.name))?;
        let cost = payload["cost"]
            .as_str()
            .ok_or_else(|| format!("{}: estimated compatibility cost is missing", self.name))?;
        let cost_fen = settlement::money::cents(cost).map_err(display_error)?;
        let creditor_id = payload["creditor_id"]
            .as_i64()
            .ok_or_else(|| format!("{}: estimated compatibility creditor is missing", self.name))?;
        let start =
            settlement::time::instant(payload["start"].as_str().ok_or_else(|| {
                format!("{}: estimated compatibility start is missing", self.name)
            })?)
            .map_err(display_error)?;
        let start_meta = json!({
            "start": start.clone(),
            "start_mode": "exact",
            "start_lower": start.clone(),
            "start_upper": null,
            "start_confirmed": 1,
            "start_confirmed_by": null,
            "start_confirmed_at": null,
            "start_source_usage_id": null,
        });
        let created_at = settlement::time::instant(
            self.workflow["clock"]["value"]
                .as_str()
                .ok_or_else(|| format!("{}: fixed compatibility clock is missing", self.name))?,
        )
        .map_err(display_error)?;
        let mut connection = open_writable(&self.data_dir.join("settlement.sqlite3")).await?;
        sqlx::query(
            "INSERT INTO profiles(account_id,name,kind) VALUES(?,?,'paid') ON CONFLICT(account_id) DO UPDATE SET name=excluded.name,kind='paid'",
        )
        .bind(account_id)
        .bind(name)
        .execute(&mut connection)
        .await
        .map_err(display_error)?;
        let result = sqlx::query(
            "INSERT INTO cycles(account_id,name,cost_fen,creditor_id,start,start_mode,start_lower,start_upper,start_confirmed,start_confirmed_by,start_confirmed_at,start_source_usage_id,renewed_from_cycle_id,start_locked,cost_kind) VALUES(?,?,?,?,?,'exact',?,NULL,1,NULL,NULL,NULL,NULL,0,'estimated')",
        )
        .bind(account_id)
        .bind(name)
        .bind(cost_fen)
        .bind(creditor_id)
        .bind(&start)
        .bind(&start)
        .execute(&mut connection)
        .await
        .map_err(display_error)?;
        let cycle_id = result.last_insert_rowid();
        let mut audit_payload = start_meta;
        audit_payload["cost_fen"] = json!(cost_fen);
        audit_payload["cost_kind"] = json!("estimated");
        audit_payload["creditor_id"] = json!(creditor_id);
        audit_payload["renewed_from_cycle_id"] = Value::Null;
        sqlx::query(
            "INSERT INTO audit(actor_id,action,entity_id,payload,created_at) VALUES(1,'create_cycle',?,?,?)",
        )
        .bind(cycle_id)
        .bind(serde_json::to_string(&audit_payload).map_err(display_error)?)
        .bind(created_at)
        .execute(&mut connection)
        .await
        .map_err(display_error)?;
        let actor = request["actor"]
            .as_i64()
            .ok_or_else(|| format!("{}: estimated compatibility actor is missing", self.name))?;
        let key = request["headers"]["Idempotency-Key"]
            .as_str()
            .ok_or_else(|| {
                format!(
                    "{}: estimated compatibility idempotency key is missing",
                    self.name
                )
            })?;
        let method = request["method"]
            .as_str()
            .ok_or_else(|| format!("{}: estimated compatibility method is missing", self.name))?;
        let path = request["path"]
            .as_str()
            .ok_or_else(|| format!("{}: estimated compatibility path is missing", self.name))?;
        let signed = mutation_signature(method, strip_api_prefix(path), request["json"].clone())
            .ok_or_else(|| {
                format!(
                    "{}: estimated compatibility signature is unsupported",
                    self.name
                )
            })?;
        let signature = sha256_hex(canonical_json(&signed).as_bytes());
        sqlx::query("INSERT INTO requests(actor_id,key,signature,response) VALUES(?,?,?,?)")
            .bind(actor)
            .bind(key)
            .bind(&signature)
            .bind(serde_json::to_string(&json!({"id": cycle_id})).map_err(display_error)?)
            .execute(&mut connection)
            .await
            .map_err(display_error)?;
        self.signatures.insert(
            (actor, key.to_owned()),
            SignatureInput {
                actual: signed.clone(),
                normalized: self.aliases.normalize_value(&signed),
            },
        );
        connection.close().await.map_err(display_error)?;
        Ok(())
    }

    fn replay_request(&self, step: &Value) -> TestResult<Value> {
        let mut request = step
            .get("request")
            .cloned()
            .ok_or_else(|| format!("{}: request missing", self.name))?;
        for binding in self.replay_overlay["request_header_bindings"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if binding["workflow"] != self.name || binding["step"] != step["label"] {
                continue;
            }
            let header = binding["header"].as_str().ok_or("overlay header missing")?;
            if binding["method"] != request["method"]
                || binding["path"] != request["path"]
                || binding["frozen_alias"] != request["headers"][header]
                || binding["producer_field"] != "csrf"
            {
                return Err(format!(
                    "{} / {}: static CSRF producer overlay does not match its frozen request",
                    self.name, step["label"]
                ));
            }
            let producer = binding["producer"]
                .as_str()
                .ok_or("overlay producer missing")?;
            let csrf = self
                .csrf_by_sso
                .get(producer)
                .ok_or_else(|| format!("{}: SSO producer {producer} has not run", self.name))?;
            request["headers"][header] = json!(csrf);
        }
        Ok(request)
    }

    async fn run_sso_step(&mut self, step: &Value) -> TestResult<()> {
        let label = step["label"].as_str().unwrap_or("sso-step");
        let request = step
            .get("request")
            .ok_or_else(|| format!("{} / {label}: request missing", self.name))?;
        let response = self.send_request(request, true).await?;
        self.bind_sso_aliases(step, request, &response).await?;
        self.compare_response(step, &response, true)?;
        self.compare_upstream_calls(step).await?;
        Ok(())
    }

    async fn wait_for_deletion_http_state(&self, step: &Value) -> TestResult<()> {
        let path = strip_api_prefix(step["request"]["path"].as_str().unwrap_or(""));
        let mut targets = Vec::new();
        if path == "/account-deletions"
            && let Some(items) = step["expect"]["body"]["items"].as_array()
        {
            for item in items {
                if matches!(item["status"].as_str(), Some("failed" | "done")) {
                    targets.push(
                        json!({"id":item["id"],"status":item["status"],"version":item["version"]}),
                    );
                }
            }
        } else if self.name == "deletion-jobs"
            && let Some(id) = path
                .strip_prefix("/account-deletions/")
                .and_then(|tail| tail.strip_suffix("/cancel"))
            && let Ok(id) = id.parse::<i64>()
        {
            // The v0.8 driver waits for this exact failed attempt before cancellation.
            targets.push(
                json!({"id":id,"status":"failed","version":step["request"]["json"]["version"]}),
            );
        }
        self.wait_for_deletion_rows(&targets, step["label"].as_str().unwrap_or("deletion-state"))
            .await
    }

    async fn wait_for_deletion_snapshot_state(&self, expected: &Value) -> TestResult<()> {
        let targets = expected["database"]["account_deletions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| matches!(item["status"].as_str(), Some("failed" | "done")))
            .map(|item| json!({"id":item["id"],"status":item["status"],"version":item["version"]}))
            .collect::<Vec<_>>();
        self.wait_for_deletion_rows(
            &targets,
            expected["label"].as_str().unwrap_or("deletion-snapshot"),
        )
        .await
    }

    async fn wait_for_deletion_rows(&self, targets: &[Value], label: &str) -> TestResult<()> {
        if targets.is_empty() {
            return Ok(());
        }
        let mut connection = open_read_only(&self.data_dir.join("settlement.sqlite3")).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let mut actual = Vec::with_capacity(targets.len());
            for expected in targets {
                let id = expected["id"].as_i64().ok_or_else(|| {
                    format!("{} / {label}: deletion target id is missing", self.name)
                })?;
                let row = sqlx::query("SELECT id,status,version FROM account_deletions WHERE id=?")
                    .bind(id)
                    .fetch_optional(&mut connection)
                    .await
                    .map_err(display_error)?;
                actual.push(match row {
                    Some(row) => json!({
                        "id":row.try_get::<i64,_>("id").map_err(display_error)?,
                        "status":row.try_get::<String,_>("status").map_err(display_error)?,
                        "version":row.try_get::<i64,_>("version").map_err(display_error)?,
                    }),
                    None => Value::Null,
                });
            }
            if actual == targets {
                connection.close().await.map_err(display_error)?;
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(json_difference(
                    &format!(
                        "{} / {label} asynchronous deletion terminal state after 10 seconds",
                        self.name
                    ),
                    &Value::Array(targets.to_vec()),
                    &Value::Array(actual),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn send_request(&mut self, request: &Value, sso: bool) -> TestResult<ObservedResponse> {
        let response = self.send_request_immutable(request, sso).await?;
        if let Some(actor) = request.get("actor").and_then(Value::as_i64)
            && let Some(raw_cookie) = response
                .headers
                .get("set-cookie")
                .and_then(|value| value.to_str().ok())
            && let Some((name, cookie)) = cookie_pair(raw_cookie)
            && name == "settlement_session"
        {
            self.actor_cookies.insert(actor, cookie);
        }
        Ok(response)
    }

    async fn send_request_immutable(
        &self,
        request: &Value,
        sso: bool,
    ) -> TestResult<ObservedResponse> {
        let method_text = request["method"]
            .as_str()
            .ok_or_else(|| format!("{}: request method missing", self.name))?;
        let method = Method::from_bytes(method_text.as_bytes()).map_err(display_error)?;
        let raw_path = request["path"]
            .as_str()
            .ok_or_else(|| format!("{}: request path missing", self.name))?;
        let path = self.aliases.expand_string(raw_path)?;
        let actor = request.get("actor").and_then(Value::as_i64);
        let mut headers = HeaderMap::new();

        let explicit_headers = request.get("headers").and_then(Value::as_object);
        let has_explicit_cookie = explicit_headers
            .is_some_and(|map| map.keys().any(|key| key.eq_ignore_ascii_case("cookie")));
        if !has_explicit_cookie
            && request.get("cookie").is_none_or(Value::is_null)
            && let Some(cookie) = actor.and_then(|id| self.actor_cookies.get(&id))
        {
            insert_header(
                &mut headers,
                "Cookie",
                &format!("settlement_session={cookie}"),
            )?;
        }

        if sso && let Some(cookie_value) = request.get("cookie").filter(|value| !value.is_null()) {
            let value = self.header_value(cookie_value)?.map(|value| {
                if value.trim_start().starts_with("settlement_session=") {
                    value
                } else {
                    format!("settlement_session={value}")
                }
            });
            if let Some(value) = value {
                insert_header(&mut headers, "Cookie", &value)?;
            }
        }

        if let Some(explicit_headers) = explicit_headers {
            for (name, value) in explicit_headers {
                let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(display_error)?;
                match self.header_value(value)? {
                    Some(value) => {
                        let header_value = HeaderValue::from_str(&value).map_err(display_error)?;
                        headers.insert(header_name, header_value);
                    }
                    None => {
                        headers.remove(header_name);
                    }
                }
            }
        }

        let url = format!("{}{}", self.origin, path);
        let mut builder = self.client.request(method.clone(), url);
        if let Some(query) = request.get("query").filter(|value| value.is_object()) {
            let query = self.aliases.expand_value(query)?;
            if !query.as_object().is_some_and(|query| query.is_empty()) {
                builder = builder.query(&query);
            }
        }
        if let Some(body) = request.get("json").filter(|value| !value.is_null()) {
            let body = self.aliases.expand_value(body)?;
            builder = builder.json(&body);
        }
        builder = builder.headers(headers);

        let response = builder
            .send()
            .await
            .map_err(|error| format!("{} {}: transport error: {error}", method_text, path))?;
        let status = response.status().as_u16();
        let response_headers = response.headers().clone();
        let content_type = response_headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let bytes = response.bytes().await.map_err(display_error)?;
        let body = if sso {
            Value::Null
        } else if bytes.is_empty() {
            Value::String(String::new())
        } else if content_type.starts_with("application/json") {
            serde_json::from_slice(&bytes)
                .map_err(|error| format!("{}: invalid JSON response body: {error}", self.name))?
        } else {
            Value::String(String::from_utf8_lossy(&bytes).into_owned())
        };

        Ok(ObservedResponse {
            status,
            body,
            headers: response_headers,
        })
    }

    fn header_value(&self, value: &Value) -> TestResult<Option<String>> {
        if value.is_null() {
            return Ok(None);
        }
        if let Some(text) = value.as_str() {
            return self.aliases.expand_string(text).map(Some);
        }
        if let Some(number) = value.as_number() {
            return Ok(Some(number.to_string()));
        }
        if let Some(object) = value.as_object()
            && let Some(cookie_alias) = object.get("cookie")
        {
            let cookie = cookie_alias
                .as_str()
                .ok_or_else(|| format!("{}: cookie alias must be a string", self.name))?;
            let mut value = self.aliases.expand_string(cookie)?;
            match object.get("mutation").and_then(Value::as_str) {
                None => {}
                Some("replace-first-character") => {
                    let mut chars = value.chars();
                    let first = chars.next().ok_or_else(|| {
                        format!("{}: cannot tamper with an empty cookie", self.name)
                    })?;
                    let replacement = if first == 'A' { 'B' } else { 'A' };
                    value = format!("{replacement}{}", chars.as_str());
                }
                Some(other) => {
                    return Err(format!(
                        "{}: unsupported cookie mutation {other:?}",
                        self.name
                    ));
                }
            }
            return Ok(Some(format!("settlement_session={value}")));
        }
        Err(format!("{}: unsupported header value {value}", self.name))
    }

    async fn bind_http_aliases(&mut self, step: &Value, body: &Value) -> TestResult<()> {
        let Some(bind) = step.get("bind").and_then(Value::as_object) else {
            return Ok(());
        };
        for (alias, source) in bind {
            let from = source
                .get("from")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{}: bind source missing", self.name))?;
            let value = value_from_bind_path(body, from)
                .ok_or_else(|| format!("{}: bind path {from:?} not found", self.name))?;
            let value = scalar_to_string(value)
                .ok_or_else(|| format!("{}: bound value at {from:?} is not scalar", self.name))?;
            self.aliases.bind(alias, value)?;
        }
        Ok(())
    }

    async fn bind_sso_aliases(
        &mut self,
        step: &Value,
        request: &Value,
        response: &ObservedResponse,
    ) -> TestResult<()> {
        let label = step["label"].as_str().unwrap_or("sso-step");
        let actor = request["actor"]
            .as_i64()
            .ok_or_else(|| format!("{} / {label}: SSO actor missing", self.name))?;
        let bind = step
            .get("bind")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{} / {label}: SSO bind object missing", self.name))?;
        let set_cookie = response
            .headers
            .get("set-cookie")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| format!("{} / {label}: Set-Cookie missing", self.name))?;
        let (cookie_name, cookie) = cookie_pair(set_cookie)
            .ok_or_else(|| format!("{} / {label}: malformed Set-Cookie", self.name))?;
        if cookie_name != "settlement_session" {
            return Err(format!(
                "{} / {label}: unexpected cookie {cookie_name:?}",
                self.name
            ));
        }
        let session_id = sha256_hex(cookie.as_bytes());
        let session =
            read_session_row(&self.data_dir.join("settlement.sqlite3"), &session_id).await?;
        let user_id = session["user_id"]
            .as_i64()
            .ok_or_else(|| format!("{} / {label}: session user_id missing", self.name))?;
        if user_id != actor {
            return Err(format!(
                "{} / {label}: cookie hash resolves to user {user_id}, expected {actor}",
                self.name
            ));
        }
        let token = request["query"]["token"]
            .as_str()
            .ok_or_else(|| format!("{} / {label}: SSO token missing", self.name))?;
        let decrypted =
            self.decrypt_session_token(session["token"].as_str().ok_or_else(|| {
                format!("{} / {label}: encrypted session token missing", self.name)
            })?)?;
        if decrypted != token {
            return Err(format!(
                "{} / {label}: session token decrypts to a different fixture identity",
                self.name
            ));
        }
        let csrf = session["csrf"]
            .as_str()
            .ok_or_else(|| format!("{} / {label}: session CSRF missing", self.name))?;
        let expires = session["expires_at"]
            .as_f64()
            .ok_or_else(|| format!("{} / {label}: session expiry missing", self.name))?;
        let vector = self.session_vector(actor, token).ok_or_else(|| {
            format!(
                "{} / {label}: no frozen synthetic session vector for actor {actor}",
                self.name
            )
        })?;
        validate_session_vector(vector, &self.name, label)?;
        if vector["synthetic"] != true {
            return Err(format!(
                "{} / {label}: session vector is not marked synthetic",
                self.name
            ));
        }
        if vector["session_id_sha256"].as_str()
            != Some(sha256_hex(vector["cookie"].as_str().unwrap_or_default().as_bytes()).as_str())
        {
            return Err(format!(
                "{} / {label}: synthetic vector cookie hash is inconsistent",
                self.name
            ));
        }
        if !valid_urlsafe_token(&cookie) || !valid_urlsafe_token(csrf) {
            return Err(format!(
                "{} / {label}: runtime cookie or CSRF token lacks fixture entropy",
                self.name
            ));
        }
        if self.clock_mode == "fixed" {
            let now = self.workflow["clock"]["value"]
                .as_str()
                .and_then(timestamp_epoch)
                .ok_or_else(|| format!("{} / {label}: fixed session clock missing", self.name))?;
            let max_age = step["expect"]["headers"]["set-cookie"]["max_age"]
                .as_f64()
                .ok_or_else(|| {
                    format!("{} / {label}: frozen session max-age missing", self.name)
                })?;
            let expected_expiry = now + max_age;
            if (expires - expected_expiry).abs() > 0.000_001 {
                return Err(format!(
                    "{} / {label}: session expiry {expires} differs from fixed clock plus frozen max-age {expected_expiry}",
                    self.name
                ));
            }
        } else {
            let runtime_now = system_epoch();
            if expires < self.started_at_epoch + 28_800.0 || expires > runtime_now + 28_810.0 {
                return Err(format!(
                    "{} / {label}: runtime session expiry falls outside the 8-hour window",
                    self.name
                ));
            }
        }

        let cookie_alias = bind["cookie"]
            .as_str()
            .ok_or_else(|| format!("{} / {label}: cookie bind alias missing", self.name))?;
        let session_alias = bind["session_id"]
            .as_str()
            .ok_or_else(|| format!("{} / {label}: session_id bind alias missing", self.name))?;
        let csrf_alias = bind["csrf"]
            .as_str()
            .ok_or_else(|| format!("{} / {label}: csrf bind alias missing", self.name))?;
        self.aliases.bind(cookie_alias, cookie.clone())?;
        if session_alias != format!("<session:{actor}>") || csrf_alias != format!("<csrf:{actor}>")
        {
            return Err(format!(
                "{} / {label}: session aliases do not name their verified actor",
                self.name
            ));
        }
        self.aliases
            .bind_session_generation(session_alias, session_id)?;
        self.aliases
            .bind_session_generation(csrf_alias, csrf.to_owned())?;
        self.csrf_by_sso.insert(label.to_owned(), csrf.to_owned());
        self.actor_cookies.insert(actor, cookie);
        Ok(())
    }

    fn session_vector(&self, actor: i64, token: &str) -> Option<&Value> {
        self.session_vectors
            .iter()
            .find(|vector| {
                vector["actor"].as_i64() == Some(actor) && vector["token"].as_str() == Some(token)
            })
            .or_else(|| {
                self.session_vectors
                    .iter()
                    .find(|vector| vector["actor"].as_i64() == Some(actor))
            })
    }

    fn clock_alias_definition(&self, alias: &str) -> Option<&Value> {
        self.workflow["clock"]["aliases"]
            .as_array()?
            .iter()
            .find(|definition| definition["alias"].as_str() == Some(alias))
    }

    fn bind_clock_aliases_from_response(&mut self, step: &Value, body: &Value) -> TestResult<()> {
        if self.clock_mode != "runtime" {
            return Ok(());
        }
        let expected = &step["expect"]["body"];
        let mut bindings = Vec::new();
        collect_response_clock_bindings(
            &self.workflow["clock"],
            expected,
            body,
            &mut bindings,
            &self.name,
            step["label"].as_str().unwrap_or("response"),
        )?;
        for (alias, value) in bindings {
            if !runtime_clock_value_matches(
                &self.workflow["clock"],
                &alias,
                &Value::String(value.clone()),
                self.started_at_epoch,
                system_epoch(),
            )? {
                return Err(format!(
                    "{} / {}: runtime clock alias {alias} is outside its frozen time window",
                    self.name,
                    step["label"].as_str().unwrap_or("response")
                ));
            }
            self.aliases.bind(&alias, value)?;
        }
        self.derive_clock_aliases()
    }

    fn derive_clock_aliases(&mut self) -> TestResult<()> {
        let Some(definitions) = self.workflow["clock"]["aliases"].as_array() else {
            return Ok(());
        };
        for definition in definitions {
            let Some(alias) = definition["alias"].as_str() else {
                continue;
            };
            let Some(from) = definition["from"].as_str() else {
                continue;
            };
            let Some(value) = self.aliases.values.get(from).cloned() else {
                continue;
            };
            let derived = match definition["kind"].as_str().unwrap_or("") {
                "wire_timestamp" => wire_datetime(Value::String(value))
                    .as_str()
                    .map(str::to_owned),
                "relative_timestamp" => {
                    let offset = definition["offset_microseconds"].as_i64().ok_or_else(|| {
                        format!(
                            "{}: relative clock alias {alias} lacks an integer offset",
                            self.name
                        )
                    })?;
                    shift_datetime(&value, offset)
                }
                _ => None,
            };
            let Some(derived) = derived else {
                if matches!(
                    definition["kind"].as_str(),
                    Some("wire_timestamp" | "relative_timestamp")
                ) {
                    return Err(format!(
                        "{}: could not derive runtime clock alias {alias}",
                        self.name
                    ));
                }
                continue;
            };
            self.aliases.bind(alias, derived)?;
        }
        Ok(())
    }

    async fn bind_runtime_usage_digests(&mut self, step: &Value, body: &Value) -> TestResult<()> {
        if self.clock_mode != "runtime" {
            return Ok(());
        }
        let Some(expected_calculations) = step["expect"]["body"]["cycles"].as_array() else {
            return Ok(());
        };
        let Some(actual_calculations) = body["cycles"].as_array() else {
            return Ok(());
        };
        let label = step["label"].as_str().unwrap_or("runtime-digest");
        let mut connection = open_read_only(&self.data_dir.join("settlement.sqlite3")).await?;
        for (expected, actual) in expected_calculations.iter().zip(actual_calculations) {
            let Some(expected_digest) = expected["digest"].as_str() else {
                continue;
            };
            let actual_digest = actual["digest"]
                .as_str()
                .ok_or_else(|| format!("{} / {label}: usage digest missing", self.name))?;
            let root_id = actual["cycle"]["account_id"]
                .as_i64()
                .ok_or_else(|| format!("{} / {label}: usage root id missing", self.name))?;
            let start = actual["cycle"]["start"]
                .as_str()
                .ok_or_else(|| format!("{} / {label}: exact usage start missing", self.name))?;
            let end = actual["end"]
                .as_str()
                .ok_or_else(|| format!("{} / {label}: exact usage end missing", self.name))?;
            let rows = sqlx::query("SELECT id,account_id,root_id,user_id,amount,created_at FROM usage WHERE root_id=? AND created_at>=? AND created_at<? ORDER BY id")
                .bind(root_id).bind(start).bind(end)
                .fetch_all(&mut connection).await.map_err(display_error)?;
            let rows = rows
                .iter()
                .map(|row| {
                    Ok(json!({
                        "id":row.try_get::<i64,_>("id").map_err(display_error)?,
                        "account_id":row.try_get::<i64,_>("account_id").map_err(display_error)?,
                        "root_id":row.try_get::<i64,_>("root_id").map_err(display_error)?,
                        "user_id":row.try_get::<i64,_>("user_id").map_err(display_error)?,
                        "amount":row.try_get::<String,_>("amount").map_err(display_error)?,
                        "created_at":row.try_get::<String,_>("created_at").map_err(display_error)?,
                    }))
                })
                .collect::<TestResult<Vec<_>>>()?;
            let rows = Value::Array(rows);
            if sha256_hex(canonical_json(&rows).as_bytes()) != actual_digest {
                return Err(format!(
                    "{} / {label}: usage digest does not hash its exact selected financial rows",
                    self.name
                ));
            }
            let normalized = self.aliases.normalize_value(&rows);
            let normalized_digest = sha256_hex(canonical_json(&normalized).as_bytes());
            if normalized_digest != expected_digest {
                return Err(format!(
                    "{} / {label}: frozen normalized usage digest differs: expected {expected_digest}, actual {normalized_digest}; normalized rows {normalized}",
                    self.name
                ));
            }
            self.aliases
                .bind(expected_digest, actual_digest.to_owned())?;
        }
        connection.close().await.map_err(display_error)?;
        Ok(())
    }

    async fn bind_netting_balance_digest(&mut self, step: &Value, body: &Value) -> TestResult<()> {
        let request = &step["request"];
        let path = request["path"].as_str().unwrap_or("");
        if request["method"] != "GET" || step["expect"]["status"] != 200 {
            return Ok(());
        }
        if path == "/settlement/api/v1/netting/balances" {
            let actual_balances = body["balances"]
                .as_array()
                .ok_or_else(|| format!("{}: netting balance list is missing", self.name))?;
            let fixture_balances = step["expect"]["body"]["balances"]
                .as_array()
                .ok_or_else(|| format!("{}: fixture netting balance list is missing", self.name))?;
            for actual in actual_balances {
                let a = actual["user_a"].as_i64().ok_or_else(|| {
                    format!("{}: netting balance list user_a is missing", self.name)
                })?;
                let b = actual["user_b"].as_i64().ok_or_else(|| {
                    format!("{}: netting balance list user_b is missing", self.name)
                })?;
                let fixture = fixture_balances
                    .iter()
                    .find(|item| item["user_a"] == a && item["user_b"] == b)
                    .ok_or_else(|| {
                        format!(
                            "{}: fixture has no netting balance for pair {a}/{b}",
                            self.name
                        )
                    })?;
                self.bind_netting_pair_digest(
                    step["label"].as_str().unwrap_or("netting-balance-list"),
                    a,
                    b,
                    fixture["digest"].as_str().ok_or_else(|| {
                        format!("{}: fixture netting balance digest is missing", self.name)
                    })?,
                    actual["digest"].as_str().ok_or_else(|| {
                        format!("{}: response netting balance digest is missing", self.name)
                    })?,
                )
                .await?;
            }
            return Ok(());
        }
        let Some(pair_path) = path.strip_prefix("/settlement/api/v1/netting/balances/") else {
            return Ok(());
        };
        let mut pair = pair_path.split('/');
        let a = pair
            .next()
            .and_then(|id| id.parse::<i64>().ok())
            .ok_or_else(|| format!("{}: netting balance user_a path id is invalid", self.name))?;
        let b = pair
            .next()
            .and_then(|id| id.parse::<i64>().ok())
            .ok_or_else(|| format!("{}: netting balance user_b path id is invalid", self.name))?;
        if pair.next().is_some() {
            return Err(format!(
                "{}: netting balance path has more than two user ids",
                self.name
            ));
        }
        let label = step["label"].as_str().unwrap_or("netting-balance-digest");
        let fixture_digest = step["expect"]["body"]["digest"]
            .as_str()
            .ok_or_else(|| format!("{} / {label}: fixture netting digest is missing", self.name))?;
        let actual_digest = body["digest"].as_str().ok_or_else(|| {
            format!(
                "{} / {label}: response netting digest is missing",
                self.name
            )
        })?;

        self.bind_netting_pair_digest(label, a, b, fixture_digest, actual_digest)
            .await
    }

    async fn bind_netting_pair_digest(
        &mut self,
        label: &str,
        a: i64,
        b: i64,
        fixture_digest: &str,
        actual_digest: &str,
    ) -> TestResult<()> {
        let mut connection = open_read_only(&self.data_dir.join("settlement.sqlite3")).await?;
        let rows = sqlx::query(
            "SELECT * FROM orders WHERE status='pending' AND ((payer_id=? AND creditor_id=?) OR (payer_id=? AND creditor_id=?)) ORDER BY id",
        )
        .bind(a)
        .bind(b)
        .bind(b)
        .bind(a)
        .fetch_all(&mut connection)
        .await
        .map_err(display_error)?;
        let mut rows = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "id":row.try_get::<i64,_>("id").map_err(display_error)?,
                    "settlement_id":row.try_get::<i64,_>("settlement_id").map_err(display_error)?,
                    "cycle_id":row.try_get::<Option<i64>,_>("cycle_id").map_err(display_error)?,
                    "expense_id":row.try_get::<Option<i64>,_>("expense_id").map_err(display_error)?,
                    "payer_id":row.try_get::<i64,_>("payer_id").map_err(display_error)?,
                    "creditor_id":row.try_get::<i64,_>("creditor_id").map_err(display_error)?,
                    "payer_name":row.try_get::<String,_>("payer_name").map_err(display_error)?,
                    "creditor_name":row.try_get::<String,_>("creditor_name").map_err(display_error)?,
                    "account_name":row.try_get::<String,_>("account_name").map_err(display_error)?,
                    "amount_fen":row.try_get::<i64,_>("amount_fen").map_err(display_error)?,
                    "status":row.try_get::<String,_>("status").map_err(display_error)?,
                    "batch_id":row.try_get::<Option<i64>,_>("batch_id").map_err(display_error)?,
                    "netting_payment_id":row.try_get::<Option<i64>,_>("netting_payment_id").map_err(display_error)?,
                    "version":row.try_get::<i64,_>("version").map_err(display_error)?,
                    "created_at":row.try_get::<String,_>("created_at").map_err(display_error)?,
                    "submitted_at":row.try_get::<Option<String>,_>("submitted_at").map_err(display_error)?,
                    "paid_at":row.try_get::<Option<String>,_>("paid_at").map_err(display_error)?,
                }))
            })
            .collect::<TestResult<Vec<_>>>()?;
        connection.close().await.map_err(display_error)?;

        let exact_rows = Value::Array(rows.clone());
        let exact_digest = sha256_hex(canonical_json(&exact_rows).as_bytes());
        if exact_digest != actual_digest {
            return Err(format!(
                "{} / {label}: response digest does not hash the complete selected pending orders (rows {exact_digest}, response {actual_digest})",
                self.name,
            ));
        }
        self.presentation.restore_fixture_order_names(&mut rows);
        let fixture_rows = self.aliases.normalize_value(&Value::Array(rows));
        if sha256_hex(canonical_json(&fixture_rows).as_bytes()) != fixture_digest {
            return Err(format!(
                "{} / {label}: source-name-reversed orders do not reproduce the frozen fixture digest",
                self.name
            ));
        }
        self.aliases.bind(fixture_digest, actual_digest.to_owned())
    }

    fn compare_expected_value(
        &self,
        expected: &Value,
        actual: &Value,
        label: &str,
    ) -> TestResult<()> {
        if let Some(alias) = expected.as_str()
            && alias.starts_with("<clock:")
            && let Some(definition) = self.clock_alias_definition(alias)
        {
            let kind = definition["kind"].as_str().unwrap_or("");
            if matches!(
                kind,
                "generated_timestamp" | "preview_expiry" | "session_expiry"
            ) {
                if self.clock_mode != "runtime"
                    || !runtime_clock_value_matches(
                        &self.workflow["clock"],
                        alias,
                        actual,
                        self.started_at_epoch,
                        system_epoch(),
                    )?
                {
                    return Err(json_difference(label, expected, actual));
                }
                return Ok(());
            }
        }

        match (expected, actual) {
            (Value::Object(expected), Value::Object(actual)) => {
                if expected.len() != actual.len()
                    || expected.keys().any(|key| !actual.contains_key(key))
                {
                    return Err(json_difference(
                        label,
                        &Value::Object(expected.clone()),
                        &Value::Object(actual.clone()),
                    ));
                }
                for (key, expected_value) in expected {
                    self.compare_expected_value(
                        expected_value,
                        &actual[key],
                        &format!("{label}.{key}"),
                    )?;
                }
                Ok(())
            }
            (Value::Array(expected), Value::Array(actual)) => {
                if expected.len() != actual.len() {
                    return Err(json_difference(
                        label,
                        &Value::Array(expected.clone()),
                        &Value::Array(actual.clone()),
                    ));
                }
                for (index, expected_value) in expected.iter().enumerate() {
                    self.compare_expected_value(
                        expected_value,
                        &actual[index],
                        &format!("{label}[{index}]"),
                    )?;
                }
                Ok(())
            }
            _ => {
                let expected = self.aliases.expand_value(expected)?;
                if &expected == actual {
                    Ok(())
                } else {
                    Err(json_difference(label, &expected, actual))
                }
            }
        }
    }

    fn decrypt_session_token(&self, ciphertext: &str) -> TestResult<String> {
        let key = self
            .session_key
            .as_deref()
            .ok_or_else(|| format!("{}: session key fixture missing", self.name))?;
        let cipher = Fernet::new(key)
            .ok_or_else(|| format!("{}: invalid synthetic Fernet key fixture", self.name))?;
        let bytes = cipher
            .decrypt(ciphertext)
            .map_err(|_| format!("{}: session token decryption failed", self.name))?;
        String::from_utf8(bytes).map_err(display_error)
    }

    fn compare_response(
        &self,
        step: &Value,
        actual: &ObservedResponse,
        sso: bool,
    ) -> TestResult<()> {
        let label = step["label"].as_str().unwrap_or("response");
        let expected = step
            .get("expect")
            .ok_or_else(|| format!("{} / {label}: expected response missing", self.name))?;
        let mut expected = expected.clone();
        self.adapt_v011_expected_response(step, &mut expected)?;
        let expected_status = expected["status"]
            .as_u64()
            .ok_or_else(|| format!("{} / {label}: expected status missing", self.name))?;
        if actual.status as u64 != expected_status {
            return Err(format!(
                "{} / {label}: HTTP {}, expected {}",
                self.name, actual.status, expected_status
            ));
        }
        let mut expected_body = expected["body"].clone();
        self.presentation
            .adapt_response_body(step, &mut expected_body)?;
        self.adapt_v011_null_vendor_response(&mut expected_body, &actual.body)?;
        self.compare_expected_value(
            &expected_body,
            &actual.body,
            &format!("{} / {label} body", self.name),
        )?;
        let expected_headers = expected["headers"]
            .as_object()
            .ok_or_else(|| format!("{} / {label}: expected headers missing", self.name))?;
        let actual_headers = self.capture_headers(&actual.headers, expected_headers, sso)?;
        let mut expanded_headers = Map::new();
        for (name, value) in expected_headers {
            let value = if name.eq_ignore_ascii_case("location") {
                value.clone()
            } else {
                self.aliases.expand_value(value)?
            };
            expanded_headers.insert(name.clone(), value);
        }
        let expected_headers = Value::Object(expanded_headers);

        let comparison = step.get("comparison").unwrap_or(&Value::Null);
        let expected_headers = apply_header_comparison_exception(
            &self.name,
            label,
            expected_status,
            comparison,
            expected_headers,
        )?;
        if expected_headers != actual_headers {
            return Err(json_difference(
                &format!("{} / {label} headers", self.name),
                &expected_headers,
                &actual_headers,
            ));
        }
        Ok(())
    }

    fn adapt_v011_expected_response(&self, step: &Value, expected: &mut Value) -> TestResult<()> {
        if !self.v011_cost_contract {
            return Ok(());
        }
        let label = step["label"].as_str().unwrap_or_default();
        if self.is_v011_estimated_create(step) {
            expected["status"] = json!(422);
            expected["body"] = json!({
                "detail": [{
                    "type": "literal_error",
                    "loc": ["body", "cost_kind"],
                    "msg": "Input should be 'exact'",
                    "input": "estimated",
                    "ctx": {"expected": "'exact'"}
                }]
            });
            return Ok(());
        }
        if matches!(
            self.name.as_str(),
            "billing-proportional" | "billing-matched-only"
        ) && label == "bill-history"
        {
            if let Some(cycles) = expected["body"]["cycles"].as_array_mut() {
                for cycle in cycles {
                    if cycle["id"] == 2 {
                        cycle["cost_kind"] = json!("exact");
                    }
                }
            }
            if let Some(audit) = expected["body"]["audit"].as_array_mut() {
                for row in audit {
                    if row["action"] == "create_cycle" && row["entity_id"] == 2 {
                        row["payload"]["cost_kind"] = json!("exact");
                    }
                }
            }
        }
        Ok(())
    }

    fn capture_headers(
        &self,
        raw: &HeaderMap,
        expected: &Map<String, Value>,
        sso: bool,
    ) -> TestResult<Value> {
        for name in SECURITY_HEADERS {
            if !expected.contains_key(*name) {
                return Err(format!(
                    "{}: expected header capture omits security header {name}",
                    self.name
                ));
            }
        }
        let mut output = Map::new();
        for name in expected.keys() {
            let value = raw.get(name.as_str()).and_then(|value| value.to_str().ok());
            let captured = if name.eq_ignore_ascii_case("set-cookie") && sso {
                value.map(parse_set_cookie).unwrap_or(Value::Null)
            } else if name.eq_ignore_ascii_case("location") {
                value
                    .map(|value| {
                        if let Some(suffix) = value.strip_prefix(&self.origin) {
                            format!("<origin>{suffix}")
                        } else {
                            value.to_owned()
                        }
                    })
                    .map(Value::String)
                    .unwrap_or(Value::Null)
            } else {
                value
                    .map(|value| Value::String(value.to_owned()))
                    .unwrap_or(Value::Null)
            };
            output.insert(name.clone(), captured);
        }
        Ok(Value::Object(output))
    }

    fn record_signature(&mut self, request: &Value, response: &ObservedResponse) -> TestResult<()> {
        if response.status != 200 {
            return Ok(());
        }
        let method = request["method"].as_str().unwrap_or("");
        if method == "GET" || method == "HEAD" {
            return Ok(());
        }
        let Some(actor) = request.get("actor").and_then(Value::as_i64) else {
            return Ok(());
        };
        let headers = request.get("headers").and_then(Value::as_object);
        let key = headers
            .and_then(|headers| {
                headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("idempotency-key"))
            })
            .and_then(|(_, value)| value.as_str());
        let Some(key) = key else {
            return Ok(());
        };
        let raw_path = request["path"].as_str().unwrap_or("");
        let path = self.aliases.expand_string(raw_path)?;
        let api_path = strip_api_prefix(&path);
        let payload = request
            .get("json")
            .filter(|value| !value.is_null())
            .map(|value| self.aliases.expand_value(value))
            .transpose()?
            .unwrap_or_else(|| json!({}));
        if let Some(signed) = mutation_signature(method, api_path, payload) {
            let normalized = self.aliases.normalize_value(&signed);
            self.signatures
                .entry((actor, key.to_owned()))
                .or_insert(SignatureInput {
                    actual: signed,
                    normalized,
                });
        }
        Ok(())
    }

    async fn compare_upstream_calls(&mut self, step: &Value) -> TestResult<()> {
        let Some(expected) = step.get("upstream_calls") else {
            return Ok(());
        };
        let expected_count = expected
            .as_array()
            .ok_or_else(|| {
                format!(
                    "{} / {}: expected upstream calls are not an array",
                    self.name, step["label"]
                )
            })?
            .len();
        let calls = self.upstream.calls().await;
        let actual = Value::Array(
            calls
                .as_array()
                .into_iter()
                .flatten()
                .skip(self.upstream_call_cursor)
                .take(expected_count)
                .cloned()
                .collect(),
        );
        if expected != &actual {
            return Err(json_difference(
                &format!("{} / {} upstream calls", self.name, step["label"]),
                expected,
                &actual,
            ));
        }
        self.upstream_call_cursor += expected_count;
        Ok(())
    }

    async fn run_db_snapshot(&mut self, step: &Value) -> TestResult<()> {
        let selected;
        let step = if let Some(candidates) = step["allowed_snapshots"].as_array() {
            let winners = serde_json::to_value(&self.race_winners).map_err(display_error)?;
            let matches = candidates
                .iter()
                .filter(|candidate| candidate["race_winners"] == winners)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(format!(
                    "{}: expected one snapshot for race winners {winners}, found {}",
                    self.name,
                    matches.len()
                ));
            }
            selected = matches[0].clone();
            &selected
        } else {
            step
        };
        let label = step["label"].as_str().unwrap_or("db-snapshot");
        let mut expected = step["expect"].clone();
        self.normalize_runtime_signature_expectations(&mut expected)?;
        self.presentation
            .set_source_state(&self.upstream.current_state().await?);
        self.presentation.adapt_database_snapshot(&mut expected);
        self.adapt_v011_null_vendor_fields(&mut expected).await?;
        if self.v011_cost_contract {
            self.adapt_v011_database_expectations(&mut expected).await?;
        }
        self.wait_for_deletion_snapshot_state(&expected).await?;
        let sql_path = step.get("sql").and_then(Value::as_str).or_else(|| {
            self.workflow["database"]["snapshots"]
                .as_array()?
                .iter()
                .find(|snapshot| snapshot["label"] == label)
                .and_then(|snapshot| snapshot.get("path").or_else(|| snapshot.get("sql")))
                .and_then(Value::as_str)
        });
        let sql = fixture_path(&self.root, sql_path, &self.name)?;
        if !sql.is_file() {
            return Err(format!(
                "{} / {label}: SQL snapshot missing at {}",
                self.name,
                sql.display()
            ));
        }
        let actual = database_snapshot(
            &self.data_dir.join("settlement.sqlite3"),
            &expected,
            SnapshotContext {
                aliases: &self.aliases,
                session_key: self.session_key.as_deref(),
                signatures: &self.signatures,
                upstream: &self.upstream,
                workflow: &self.name,
                clock: &self.workflow["clock"],
                started_at_epoch: self.started_at_epoch,
            },
        )
        .await?;
        self.adapt_v011_null_vendor_snapshot(&mut expected, &actual)?;
        if actual["database"] != expected["database"] {
            return Err(json_difference(
                &format!("{} / {label} database rows", self.name),
                &expected["database"],
                &actual["database"],
            ));
        }
        if actual["schema"] != expected["schema"] {
            return Err(json_difference(
                &format!("{} / {label} schema", self.name),
                &expected["schema"],
                &actual["schema"],
            ));
        }
        let integrity = integrity_check(&self.data_dir.join("settlement.sqlite3")).await?;
        if integrity != step["integrity_check"] {
            return Err(json_difference(
                &format!("{} / {label} integrity check", self.name),
                &step["integrity_check"],
                &integrity,
            ));
        }
        let foreign_keys = foreign_key_check(&self.data_dir.join("settlement.sqlite3")).await?;
        if foreign_keys != step["foreign_key_check"] {
            return Err(json_difference(
                &format!("{} / {label} foreign key check", self.name),
                &step["foreign_key_check"],
                &foreign_keys,
            ));
        }
        let sequences = sqlite_sequences(&self.data_dir.join("settlement.sqlite3")).await?;
        if sequences != step["sequences"] {
            return Err(json_difference(
                &format!("{} / {label} SQLite sequences", self.name),
                &step["sequences"],
                &sequences,
            ));
        }
        self.compare_upstream_calls(step).await?;
        Ok(())
    }

    async fn adapt_v011_database_expectations(&self, expected: &mut Value) -> TestResult<()> {
        if matches!(
            self.name.as_str(),
            "billing-proportional" | "billing-matched-only"
        ) {
            if let Some(cycles) = expected["database"]["cycles"].as_array_mut() {
                for cycle in cycles {
                    if cycle["id"] == 2 {
                        cycle["cost_kind"] = json!("exact");
                    }
                }
            }
            if let Some(audit) = expected["database"]["audit"].as_array_mut() {
                for row in audit {
                    if row["action"] == "create_cycle" && row["entity_id"] == 2 {
                        row["payload"]["cost_kind"] = json!("exact");
                    }
                }
            }
        }
        Ok(())
    }

    /// v8 API responses expose the nullable vendor associations on cycle and
    /// profile projections. Frozen v0.8 response snapshots may omit those
    /// fields only while the actual value remains NULL.
    fn adapt_v011_null_vendor_response(
        &self,
        expected: &mut Value,
        actual: &Value,
    ) -> TestResult<()> {
        fn visit(
            expected: &mut Value,
            actual: &Value,
            path: &str,
            workflow: &str,
        ) -> TestResult<()> {
            match (expected, actual) {
                (Value::Object(expected), Value::Object(actual)) => {
                    for field in ["vendor_id", "vendor_period_id"] {
                        let Some(actual_value) = actual.get(field) else {
                            continue;
                        };
                        if expected.contains_key(field) {
                            continue;
                        }
                        if !actual_value.is_null() {
                            return Err(format!(
                                "{workflow}: frozen compatibility cannot omit non-NULL API field {path}.{field}"
                            ));
                        }
                        expected.insert(field.to_owned(), Value::Null);
                    }
                    let keys = expected.keys().cloned().collect::<Vec<_>>();
                    for key in keys {
                        let Some(actual_value) = actual.get(&key) else {
                            continue;
                        };
                        let expected_value = expected
                            .get_mut(&key)
                            .expect("key collected from expected object");
                        visit(
                            expected_value,
                            actual_value,
                            &format!("{path}.{key}"),
                            workflow,
                        )?;
                    }
                }
                (Value::Array(expected), Value::Array(actual)) => {
                    for (index, (expected_value, actual_value)) in
                        expected.iter_mut().zip(actual).enumerate()
                    {
                        visit(
                            expected_value,
                            actual_value,
                            &format!("{path}[{index}]"),
                            workflow,
                        )?;
                    }
                }
                _ => {}
            }
            Ok(())
        }

        visit(expected, actual, "body", &self.name)
    }

    fn adapt_v011_null_vendor_snapshot(
        &self,
        expected: &mut Value,
        actual: &Value,
    ) -> TestResult<()> {
        fn visit(
            expected: &mut Value,
            actual: &Value,
            path: &str,
            workflow: &str,
        ) -> TestResult<()> {
            match (expected, actual) {
                (Value::Object(expected), Value::Object(actual)) => {
                    for field in ["vendor_id", "vendor_period_id"] {
                        let Some(actual_value) = actual.get(field) else {
                            continue;
                        };
                        if expected.contains_key(field) {
                            continue;
                        }
                        if !actual_value.is_null() {
                            return Err(format!(
                                "{workflow}: frozen compatibility cannot omit non-NULL database field {path}.{field}"
                            ));
                        }
                        expected.insert(field.to_owned(), Value::Null);
                    }
                    let keys = expected.keys().cloned().collect::<Vec<_>>();
                    for key in keys {
                        let Some(actual_value) = actual.get(&key) else {
                            continue;
                        };
                        let expected_value = expected
                            .get_mut(&key)
                            .expect("key collected from expected object");
                        visit(
                            expected_value,
                            actual_value,
                            &format!("{path}.{key}"),
                            workflow,
                        )?;
                    }
                }
                (Value::Array(expected), Value::Array(actual)) => {
                    for (index, (expected_value, actual_value)) in
                        expected.iter_mut().zip(actual).enumerate()
                    {
                        visit(
                            expected_value,
                            actual_value,
                            &format!("{path}[{index}]"),
                            workflow,
                        )?;
                    }
                }
                _ => {}
            }
            Ok(())
        }

        visit(
            &mut expected["database"],
            &actual["database"],
            "database",
            &self.name,
        )
    }

    /// v8 adds nullable vendor associations. Frozen v0.8 snapshots may omit
    /// those columns only when every value is still NULL; a non-NULL value is
    /// an explicit contract change and must fail the snapshot instead of being
    /// hidden by compatibility normalization.
    async fn adapt_v011_null_vendor_fields(&self, expected: &mut Value) -> TestResult<()> {
        let path = self.data_dir.join("settlement.sqlite3");
        let mut connection = open_read_only(&path).await?;
        for (table, fields) in [
            ("profiles", ["vendor_id"].as_slice()),
            ("cycles", ["vendor_id", "vendor_period_id"].as_slice()),
        ] {
            let info = sqlx::query(&format!("PRAGMA table_info(\"{table}\")"))
                .fetch_all(&mut connection)
                .await
                .map_err(display_error)?;
            for field in fields {
                let Some(column) = info.iter().find(|row| {
                    row.try_get::<String, _>("name")
                        .ok()
                        .is_some_and(|name| name == *field)
                }) else {
                    return Err(format!(
                        "{}: schema8 column {table}.{field} is missing",
                        self.name
                    ));
                };
                let _ = column;
                let non_null: i64 = sqlx::query_scalar(&format!(
                    "SELECT COUNT(*) FROM \"{table}\" WHERE \"{field}\" IS NOT NULL"
                ))
                .fetch_one(&mut connection)
                .await
                .map_err(display_error)?;
                if non_null != 0 {
                    return Err(format!(
                        "{}: frozen compatibility cannot omit non-NULL schema8 field {table}.{field}",
                        self.name
                    ));
                }
            }
            let schema = expected["schema"][table]
                .as_array_mut()
                .ok_or_else(|| format!("{}: expected schema for {table} is missing", self.name))?;
            for row in &info {
                let name = row.try_get::<String, _>("name").map_err(display_error)?;
                if fields.contains(&name.as_str())
                    && !schema.iter().any(|item| item["name"] == name)
                {
                    schema.push(json!({
                        "cid": row.try_get::<i64, _>("cid").map_err(display_error)?,
                        "name": name,
                        "type": row.try_get::<String, _>("type").map_err(display_error)?,
                        "notnull": row.try_get::<i64, _>("notnull").map_err(display_error)?,
                        "dflt_value": row.try_get::<Option<String>, _>("dflt_value").map_err(display_error)?,
                        "pk": row.try_get::<i64, _>("pk").map_err(display_error)?,
                    }));
                }
            }
            let rows = expected["database"][table]
                .as_array_mut()
                .ok_or_else(|| format!("{}: expected rows for {table} are missing", self.name))?;
            for row in rows {
                for field in fields {
                    row[*field] = Value::Null;
                }
            }
        }
        connection.close().await.map_err(display_error)
    }

    async fn current_settlement_ids(&self) -> TestResult<HashSet<i64>> {
        let mut connection = open_read_only(&self.data_dir.join("settlement.sqlite3")).await?;
        let rows = sqlx::query("SELECT id FROM settlements")
            .fetch_all(&mut connection)
            .await
            .map_err(display_error)?;
        let ids = rows
            .iter()
            .map(|row| row.try_get::<i64, _>("id").map_err(display_error))
            .collect::<TestResult<HashSet<_>>>()?;
        connection.close().await.map_err(display_error)?;
        Ok(ids)
    }

    async fn current_order_settlement_ids(&self) -> TestResult<Vec<(i64, i64)>> {
        let mut connection = open_read_only(&self.data_dir.join("settlement.sqlite3")).await?;
        let rows =
            sqlx::query("SELECT id,settlement_id FROM orders WHERE settlement_id IS NOT NULL")
                .fetch_all(&mut connection)
                .await
                .map_err(display_error)?;
        let rows = rows
            .iter()
            .map(|row| {
                Ok((
                    row.try_get::<i64, _>("id").map_err(display_error)?,
                    row.try_get::<i64, _>("settlement_id")
                        .map_err(display_error)?,
                ))
            })
            .collect::<TestResult<Vec<_>>>()?;
        connection.close().await.map_err(display_error)?;
        Ok(rows)
    }

    async fn current_settled_cycle_ids(&self) -> TestResult<HashSet<i64>> {
        let mut connection = open_read_only(&self.data_dir.join("settlement.sqlite3")).await?;
        let rows = sqlx::query("SELECT id FROM cycles WHERE status='settled'")
            .fetch_all(&mut connection)
            .await
            .map_err(display_error)?;
        let ids = rows
            .iter()
            .map(|row| row.try_get::<i64, _>("id").map_err(display_error))
            .collect::<TestResult<HashSet<_>>>()?;
        connection.close().await.map_err(display_error)?;
        Ok(ids)
    }

    fn normalize_runtime_signature_expectations(&self, expected: &mut Value) -> TestResult<()> {
        for entry in self.replay_overlay["runtime_signatures"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if entry["workflow"] != self.name {
                continue;
            }
            let actor = entry["actor_id"]
                .as_i64()
                .ok_or("runtime signature actor missing")?;
            let key = entry["idempotency_key"]
                .as_str()
                .ok_or("runtime signature key missing")?;
            let Some(row) = expected["database"]["requests"]
                .as_array_mut()
                .and_then(|rows| {
                    rows.iter_mut()
                        .find(|row| row["actor_id"].as_i64() == Some(actor) && row["key"] == key)
                })
            else {
                continue;
            };
            let source_step = self.workflow["steps"]
                .as_array()
                .and_then(|steps| steps.iter().find(|step| step["label"] == entry["step"]))
                .ok_or("runtime signature source step missing")?;
            let request = &source_step["request"];
            if entry["legacy_database"]["synthetic"] != true
                || row["signature"] != entry["raw_signature"]
                || entry["method"] != request["method"]
                || entry["path"] != request["path"]
                || request["actor"].as_i64() != Some(actor)
                || request["headers"]["Idempotency-Key"] != key
            {
                return Err(format!(
                    "{} / {key}: runtime signature provenance does not match the frozen request and row",
                    self.name
                ));
            }
            let mut legacy_aliases = AliasRegistry::default();
            for alias in entry["clock_aliases"]
                .as_array()
                .ok_or("runtime signature clock aliases missing")?
            {
                let name = alias["alias"]
                    .as_str()
                    .ok_or("runtime signature clock alias missing")?;
                if self.clock_alias_definition(name).is_none() {
                    return Err(format!(
                        "{} / {key}: undeclared clock alias {name}",
                        self.name
                    ));
                }
                let value = alias["value"]
                    .as_str()
                    .ok_or("runtime signature clock value missing")?;
                legacy_aliases.bind(name, value.to_owned())?;
            }
            let raw_payload = &entry["raw_request_json"];
            if legacy_aliases.normalize_value(raw_payload) != request["json"] {
                return Err(format!(
                    "{} / {key}: captured raw request is not the exact frozen request after named clock substitution",
                    self.name
                ));
            }
            let signed = mutation_signature(
                entry["method"]
                    .as_str()
                    .ok_or("runtime signature method missing")?,
                strip_api_prefix(
                    entry["path"]
                        .as_str()
                        .ok_or("runtime signature path missing")?,
                ),
                raw_payload.clone(),
            )
            .ok_or("runtime signature action unsupported")?;
            if sha256_hex(canonical_json(&signed).as_bytes())
                != entry["raw_signature"].as_str().unwrap_or("")
            {
                return Err(format!(
                    "{} / {key}: legacy raw signature does not hash its captured request",
                    self.name
                ));
            }
            let normalized = legacy_aliases.normalize_value(&signed);
            let actual = self
                .signatures
                .get(&(actor, key.to_owned()))
                .ok_or("runtime signature replay record missing")?;
            if actual.normalized != normalized {
                return Err(json_difference(
                    &format!("{} / {key} canonical request", self.name),
                    &normalized,
                    &actual.normalized,
                ));
            }
            row["signature"] = json!(sha256_hex(canonical_json(&normalized).as_bytes()));
        }
        Ok(())
    }
}

struct ObservedResponse {
    status: u16,
    body: Value,
    headers: HeaderMap,
}

#[derive(Default)]
struct AliasRegistry {
    values: BTreeMap<String, String>,
    reverse: HashMap<String, String>,
}

impl AliasRegistry {
    fn bind_session_generation(&mut self, alias: &str, value: String) -> TestResult<()> {
        if !((alias.starts_with("<session:") || alias.starts_with("<csrf:"))
            && alias.ends_with('>'))
        {
            return Err(format!("unexpected SSO generation alias {alias}"));
        }
        if let Some(previous) = self.reverse.get(&value)
            && previous != alias
        {
            return Err(format!(
                "one session value refers to both {previous} and {alias}"
            ));
        }
        self.reverse.insert(value.clone(), alias.to_owned());
        self.values.insert(alias.to_owned(), value);
        Ok(())
    }

    fn bind(&mut self, alias: &str, value: String) -> TestResult<()> {
        if let Some(previous) = self.values.get(alias) {
            if previous != &value {
                if !alias.starts_with("<preview:") {
                    return Err(format!("alias {alias} was rebound to a different value"));
                }
            } else {
                return Ok(());
            }
        }
        self.reverse
            .entry(value.clone())
            .or_insert_with(|| alias.to_owned());
        self.values.insert(alias.to_owned(), value);
        Ok(())
    }

    fn expand_string(&self, input: &str) -> TestResult<String> {
        if let Some(value) = self.values.get(input) {
            return Ok(value.clone());
        }
        let mut output = input.to_owned();
        for (alias, value) in &self.values {
            if alias.starts_with('<') && alias.ends_with('>') {
                output = output.replace(alias, value);
            }
        }
        Ok(output)
    }

    fn expand_value(&self, value: &Value) -> TestResult<Value> {
        match value {
            Value::String(text) => Ok(Value::String(self.expand_string(text)?)),
            Value::Array(values) => values
                .iter()
                .map(|value| self.expand_value(value))
                .collect::<TestResult<Vec<_>>>()
                .map(Value::Array),
            Value::Object(values) => values
                .iter()
                .map(|(key, value)| Ok((key.clone(), self.expand_value(value)?)))
                .collect::<TestResult<Map<_, _>>>()
                .map(Value::Object),
            other => Ok(other.clone()),
        }
    }

    fn normalize_value(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => self
                .reverse
                .get(text)
                .cloned()
                .map(Value::String)
                .unwrap_or_else(|| value.clone()),
            Value::Array(values) => Value::Array(
                values
                    .iter()
                    .map(|value| self.normalize_value(value))
                    .collect(),
            ),
            Value::Object(values) => Value::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), self.normalize_value(value)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

struct SignatureInput {
    actual: Value,
    normalized: Value,
}

#[derive(Clone)]
struct MockState {
    source_path: PathBuf,
    inner: std::sync::Arc<Mutex<MockInner>>,
}

struct MockInner {
    users: HashMap<String, Value>,
    status_overrides: HashMap<String, u16>,
    unavailable: bool,
    delete_failures: HashSet<i64>,
    calls: Vec<Value>,
    delete_delay: Duration,
    delayed_deletes: Vec<DelayedDeleteObservation>,
}

struct DelayedDeleteObservation {
    account_id: i64,
    source_before: TestResult<Value>,
    source_after: TestResult<Value>,
}

struct UpstreamServer {
    state: MockState,
    origin: String,
    stop: CancellationToken,
    task: JoinHandle<std::io::Result<()>>,
}

impl UpstreamServer {
    async fn start(source_path: PathBuf) -> TestResult<Self> {
        let source = read_json(&source_path)?;
        let users = users_from_source(&source);
        let state = MockState {
            source_path,
            inner: std::sync::Arc::new(Mutex::new(MockInner {
                users,
                status_overrides: HashMap::new(),
                unavailable: false,
                delete_failures: HashSet::new(),
                calls: Vec::new(),
                delete_delay: Duration::ZERO,
                delayed_deletes: Vec::new(),
            })),
        };
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(display_error)?;
        let address = listener.local_addr().map_err(display_error)?;
        let router = Router::new()
            .route("/api/v1/auth/me", get(upstream_auth_me))
            .route(
                "/api/v1/admin/accounts/{id}",
                delete(upstream_delete_account),
            )
            .with_state(state.clone());
        let stop = CancellationToken::new();
        let signal = stop.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(signal.cancelled_owned())
                .await
        });
        Ok(Self {
            state,
            origin: format!("http://{address}"),
            stop,
            task,
        })
    }

    fn origin(&self) -> &str {
        &self.origin
    }

    async fn set_state(&self, state: &Value) -> TestResult<()> {
        if let Some(source) = state.get("source") {
            write_json_atomic(&self.state.source_path, source)?;
        }
        let verifier = state.get("verifier").unwrap_or(&Value::Null);
        let mut inner = self.state.inner.lock().await;
        if let Some(users) = verifier.get("users") {
            inner.users = users_from_verifier(users);
        }
        inner.status_overrides = verifier
            .get("status_overrides")
            .and_then(Value::as_object)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|(token, status)| {
                        status.as_u64().map(|status| (token.clone(), status as u16))
                    })
                    .collect()
            })
            .unwrap_or_default();
        inner.unavailable = verifier
            .get("unavailable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        inner.delete_failures = verifier
            .get("delete_failures")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default();
        if state.get("reset_calls").and_then(Value::as_bool) == Some(true) {
            inner.calls.clear();
        }
        Ok(())
    }

    async fn calls(&self) -> Value {
        Value::Array(self.state.inner.lock().await.calls.clone())
    }

    async fn current_state(&self) -> TestResult<Value> {
        let source = read_json(&self.state.source_path)?;
        let inner = self.state.inner.lock().await;
        let mut delete_failures = inner.delete_failures.iter().copied().collect::<Vec<_>>();
        delete_failures.sort_unstable();
        Ok(json!({
            "source": source,
            "verifier": {
                "users": inner.users,
                "status_overrides": inner.status_overrides,
                "unavailable": inner.unavailable,
                "delete_failures": delete_failures,
            }
        }))
    }

    async fn user_id_for_token(&self, token: &str) -> Option<i64> {
        self.state
            .inner
            .lock()
            .await
            .users
            .get(token)
            .and_then(|user| user["id"].as_i64())
    }

    async fn stop(&mut self) -> TestResult<()> {
        self.stop.cancel();
        match timeout(Duration::from_secs(5), &mut self.task).await {
            Ok(joined) => {
                joined.map_err(display_error)?.map_err(display_error)?;
                Ok(())
            }
            Err(_) => {
                self.task.abort();
                Err("mock upstream shutdown timed out".to_owned())
            }
        }
    }
}

impl Drop for UpstreamServer {
    fn drop(&mut self) {
        self.stop.cancel();
        self.task.abort();
    }
}

async fn upstream_auth_me(State(state): State<MockState>, headers: HeaderMap) -> Response {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    let (user, status, unavailable) = {
        let mut inner = state.inner.lock().await;
        let status = inner
            .status_overrides
            .get(&token)
            .copied()
            .unwrap_or_else(|| {
                if inner.users.contains_key(&token) {
                    200
                } else {
                    401
                }
            });
        let unavailable = inner.unavailable;
        let user = inner.users.get(&token).cloned();
        inner.calls.push(json!({
            "method":"GET",
            "path":"/api/v1/auth/me",
            "token":token,
            "status":if unavailable { json!("transport_error") } else { json!(status) }
        }));
        (user, status, unavailable)
    };
    if unavailable {
        let body = Body::from_stream(futures_util::stream::once(async {
            Err::<Bytes, _>(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "synthetic verifier outage",
            ))
        }));
        return Response::new(body);
    }
    let body = json!({
        "code": if status == 200 { 0 } else { status },
        "data": user
    });
    json_response(status, body)
}

async fn upstream_delete_account(
    State(state): State<MockState>,
    AxumPath(account_id): AxumPath<i64>,
    headers: HeaderMap,
) -> Response {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    let (actor, status, body, delay) = {
        let mut inner = state.inner.lock().await;
        let actor = inner.users.get(&token).cloned().unwrap_or(Value::Null);
        let status = if actor["role"] != "admin" {
            403
        } else if inner.delete_failures.contains(&account_id) {
            503
        } else {
            200
        };
        let body = if status == 200 {
            json!({"code":0,"data":{}})
        } else {
            json!({"code":status})
        };
        inner.calls.push(json!({
            "method":"DELETE",
            "path":format!("/api/v1/admin/accounts/{account_id}"),
            "status":status,
            "body":{"code":if status == 200 { 0 } else { status }}
        }));
        (actor, status, body, inner.delete_delay)
    };
    let _ = actor;
    if status == 200 && !delay.is_zero() {
        let source_before = read_json(&state.source_path);
        tokio::time::sleep(delay).await;
        let source_after = read_json(&state.source_path);
        state
            .inner
            .lock()
            .await
            .delayed_deletes
            .push(DelayedDeleteObservation {
                account_id,
                source_before,
                source_after,
            });
    }
    if status == 200
        && let Ok(source) = read_json(&state.source_path)
    {
        let mut updated = source;
        let mut removed = HashSet::from([account_id]);
        loop {
            let before = removed.len();
            if let Some(accounts) = updated["accounts"].as_array() {
                for account in accounts {
                    if account["parent_account_id"]
                        .as_i64()
                        .is_some_and(|parent| removed.contains(&parent))
                        && let Some(id) = account["id"].as_i64()
                    {
                        removed.insert(id);
                    }
                }
            }
            if removed.len() == before {
                break;
            }
        }
        if let Some(accounts) = updated["accounts"].as_array_mut() {
            accounts.retain(|account| {
                account["id"]
                    .as_i64()
                    .is_none_or(|id| !removed.contains(&id))
            });
        }
        let _ = write_json_atomic(&state.source_path, &updated);
    }
    json_response(status, body)
}

fn json_response(status: u16, body: Value) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(body)).into_response()
}

fn users_from_source(source: &Value) -> HashMap<String, Value> {
    let mut users = HashMap::new();
    if let Some(rows) = source["users"].as_array() {
        for row in rows {
            let Some(id) = row["id"].as_i64() else {
                continue;
            };
            let token = format!("fixture-user-{id}");
            let mut user = row.as_object().cloned().unwrap_or_default();
            if !user.contains_key("username")
                && let Some(name) = user.get("name").cloned()
            {
                user.insert("username".to_owned(), name);
            }
            users.insert(token, Value::Object(user));
        }
    }
    users
}

fn users_from_verifier(users: &Value) -> HashMap<String, Value> {
    let mut output = HashMap::new();
    if let Some(users) = users.as_object() {
        for (token, user) in users {
            let mut user = user.as_object().cloned().unwrap_or_default();
            if !user.contains_key("username")
                && let Some(name) = user.get("name").cloned()
            {
                user.insert("username".to_owned(), name);
            }
            output.insert(token.clone(), Value::Object(user));
        }
    }
    output
}

fn write_json_atomic(path: &Path, value: &Value) -> TestResult<()> {
    let temporary = path.with_extension("next.json");
    let bytes = serde_json::to_vec(value).map_err(display_error)?;
    fs::write(&temporary, bytes).map_err(display_error)?;
    fs::rename(&temporary, path).map_err(display_error)
}

fn read_json(path: &Path) -> TestResult<Value> {
    let contents = fs::read(path).map_err(display_error)?;
    serde_json::from_slice(&contents)
        .map_err(|error| format!("{}: invalid JSON fixture: {error}", path.display()))
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn fixture_path(root: &Path, relative: Option<&str>, workflow: &str) -> TestResult<PathBuf> {
    let relative = relative.ok_or_else(|| format!("{workflow}: fixture path missing"))?;
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "{workflow}: unsafe fixture-relative path {relative:?}"
        ));
    }
    Ok(root.join(path))
}

async fn seed_database(path: &Path, fixture: &Path) -> TestResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(display_error)?;
    }
    let sql = fs::read_to_string(fixture).map_err(display_error)?;
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .map_err(display_error)?;
    sqlx::raw_sql(&sql)
        .execute(&mut connection)
        .await
        .map_err(|error| {
            format!(
                "could not load static SQL seed {}: {error}",
                fixture.display()
            )
        })?;
    connection.close().await.map_err(display_error)
}

fn load_session_vectors(root: &Path, workflow: &str) -> TestResult<Vec<Value>> {
    let path = root.join("session").join(format!("{workflow}.json"));
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let fixture = read_json(&path)?;
    if fixture["schema_version"] != "settlement-session-vectors/v1" {
        return Err(format!(
            "{workflow}: unsupported synthetic session vector schema"
        ));
    }
    let vectors = fixture["vectors"]
        .as_array()
        .ok_or_else(|| format!("{workflow}: session vectors is not an array"))?;
    for vector in vectors {
        if vector["workflow"]
            .as_str()
            .is_some_and(|value| value != workflow)
        {
            return Err(format!(
                "{workflow}: session vector belongs to another workflow"
            ));
        }
        validate_session_vector(vector, workflow, "fixture-load")?;
    }
    Ok(vectors.clone())
}

fn validate_session_vector(vector: &Value, workflow: &str, label: &str) -> TestResult<()> {
    let key = vector["fernet_key"]
        .as_str()
        .ok_or_else(|| format!("{workflow} / {label}: synthetic Fernet key missing"))?;
    let ciphertext = vector["fernet_ciphertext"]
        .as_str()
        .ok_or_else(|| format!("{workflow} / {label}: synthetic Fernet ciphertext missing"))?;
    let token = vector["token"]
        .as_str()
        .ok_or_else(|| format!("{workflow} / {label}: synthetic token missing"))?;
    let cipher = Fernet::new(key)
        .ok_or_else(|| format!("{workflow} / {label}: invalid synthetic Fernet key"))?;
    let decrypted = cipher
        .decrypt(ciphertext)
        .map_err(|_| format!("{workflow} / {label}: synthetic Fernet ciphertext is invalid"))?;
    if decrypted != token.as_bytes() {
        return Err(format!(
            "{workflow} / {label}: synthetic ciphertext/token mismatch"
        ));
    }
    let cookie = vector["cookie"]
        .as_str()
        .ok_or_else(|| format!("{workflow} / {label}: synthetic cookie missing"))?;
    let expected_hash = vector["session_id_sha256"]
        .as_str()
        .ok_or_else(|| format!("{workflow} / {label}: synthetic cookie hash missing"))?;
    if sha256_hex(cookie.as_bytes()) != expected_hash || !valid_urlsafe_token(cookie) {
        return Err(format!(
            "{workflow} / {label}: synthetic cookie/hash is inconsistent"
        ));
    }
    let csrf = vector["csrf"]
        .as_str()
        .ok_or_else(|| format!("{workflow} / {label}: synthetic CSRF token missing"))?;
    if !valid_urlsafe_token(csrf) {
        return Err(format!(
            "{workflow} / {label}: synthetic CSRF token has insufficient entropy"
        ));
    }
    Ok(())
}

fn write_session_key(data_dir: &Path, vectors: &[Value]) -> TestResult<Option<String>> {
    let Some(first) = vectors.first() else {
        return Ok(None);
    };
    let key = first["fernet_key"]
        .as_str()
        .ok_or_else(|| "synthetic session vector has no Fernet key".to_owned())?
        .to_owned();
    if vectors
        .iter()
        .any(|vector| vector["fernet_key"].as_str() != Some(key.as_str()))
    {
        return Err("one workflow uses more than one synthetic Fernet key".to_owned());
    }
    let path = data_dir.join("session.key");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(display_error)?;
    file.write_all(key.as_bytes()).map_err(display_error)?;
    file.sync_all().map_err(display_error)?;
    Ok(Some(key))
}

fn free_port() -> TestResult<u16> {
    let listener = StdTcpListener::bind(("127.0.0.1", 0)).map_err(display_error)?;
    let port = listener.local_addr().map_err(display_error)?.port();
    drop(listener);
    Ok(port)
}

fn system_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn read_log_tail(path: &Path) -> String {
    let contents = fs::read_to_string(path).unwrap_or_default();
    let mut lines = contents.lines().rev().take(30).collect::<Vec<_>>();
    lines.reverse();
    lines.join("\n")
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) -> TestResult<()> {
    let name = HeaderName::from_bytes(name.as_bytes()).map_err(display_error)?;
    let value = HeaderValue::from_str(value).map_err(display_error)?;
    headers.insert(name, value);
    Ok(())
}

fn cookie_pair(set_cookie: &str) -> Option<(String, String)> {
    let first = set_cookie.split(';').next()?.trim();
    let (name, value) = first.split_once('=')?;
    Some((name.trim().to_owned(), value.trim().to_owned()))
}

fn parse_set_cookie(set_cookie: &str) -> Value {
    let mut parts = set_cookie.split(';').map(str::trim);
    let mut cookie = Map::new();
    if let Some((name, value)) = parts.next().and_then(|part| part.split_once('=')) {
        cookie.insert("name".to_owned(), Value::String(name.to_owned()));
        cookie.insert("value".to_owned(), Value::String(value.to_owned()));
    }
    let mut httponly = false;
    let mut secure = false;
    for part in parts {
        if part.eq_ignore_ascii_case("httponly") {
            httponly = true;
        } else if part.eq_ignore_ascii_case("secure") {
            secure = true;
        } else if let Some((name, value)) = part.split_once('=') {
            match name.trim().to_ascii_lowercase().as_str() {
                "path" => {
                    cookie.insert("path".to_owned(), Value::String(value.trim().to_owned()));
                }
                "samesite" => {
                    cookie.insert(
                        "samesite".to_owned(),
                        Value::String(value.trim().to_ascii_lowercase()),
                    );
                }
                "max-age" => {
                    let parsed = value
                        .trim()
                        .parse::<i64>()
                        .map(Value::from)
                        .unwrap_or(Value::Null);
                    cookie.insert("max_age".to_owned(), parsed);
                }
                _ => {}
            }
        }
    }
    cookie.insert("httponly".to_owned(), Value::Bool(httponly));
    cookie.insert("secure".to_owned(), Value::Bool(secure));
    Value::Object(cookie)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_urlsafe_token(value: &str) -> bool {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    URL_SAFE_NO_PAD
        .decode(value)
        .is_ok_and(|bytes| bytes.len() >= 32)
}

async fn read_session_row(path: &Path, session_id: &str) -> TestResult<Value> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .map_err(display_error)?;
    let row = sqlx::query("SELECT id,user_id,token,csrf,expires_at FROM sessions WHERE id=?")
        .bind(session_id)
        .fetch_optional(&mut connection)
        .await
        .map_err(display_error)?
        .ok_or_else(|| "SSO cookie hash is absent from sessions".to_owned())?;
    let result = json!({
        "id": row.try_get::<String, _>("id").map_err(display_error)?,
        "user_id": row.try_get::<i64, _>("user_id").map_err(display_error)?,
        "token": row.try_get::<String, _>("token").map_err(display_error)?,
        "csrf": row.try_get::<String, _>("csrf").map_err(display_error)?,
        "expires_at": row.try_get::<f64, _>("expires_at").map_err(display_error)?
    });
    connection.close().await.map_err(display_error)?;
    Ok(result)
}

fn value_from_bind_path<'a>(body: &'a Value, source: &str) -> Option<&'a Value> {
    let source = source
        .strip_prefix("expect.body")
        .unwrap_or(source)
        .trim_start_matches('.');
    if source.is_empty() {
        return Some(body);
    }
    source.split('.').try_fold(body, |value, part| {
        if let Ok(index) = part.parse::<usize>() {
            value.as_array()?.get(index)
        } else {
            value.get(part)
        }
    })
}

fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn json_difference(label: &str, expected: &Value, actual: &Value) -> String {
    let expected = serde_json::to_string_pretty(expected).unwrap_or_else(|_| expected.to_string());
    let actual = serde_json::to_string_pretty(actual).unwrap_or_else(|_| actual.to_string());
    format!("{label}: mismatch\nexpected: {expected}\nactual: {actual}")
}

fn apply_header_comparison_exception(
    workflow: &str,
    label: &str,
    status: u64,
    comparison: &Value,
    expected_headers: Value,
) -> TestResult<Value> {
    let exception_case = workflow == "atomic-rollback"
        && matches!(
            label,
            "payment-mid-transaction-failure" | "incomplete-sync-failure"
        );
    if !exception_case {
        if !comparison.is_null() {
            return Err(format!(
                "{workflow} / {label}: unexpected header comparison exception"
            ));
        }
        return Ok(expected_headers);
    }
    if status != 503
        || comparison["allowed_difference"] != "legacy_unhandled_503_security_headers"
        || comparison["status_body_and_database"] != "exact"
    {
        return Err(format!(
            "{workflow} / {label}: malformed legacy 503 exception annotation"
        ));
    }
    let mut legacy_headers = Map::new();
    for name in SECURITY_HEADERS {
        legacy_headers.insert((*name).to_owned(), Value::Null);
    }
    if comparison["python_observed_headers"] != Value::Object(legacy_headers) {
        return Err(format!(
            "{workflow} / {label}: legacy null-header evidence changed"
        ));
    }
    let hardened = HARDENED_ERROR_HEADERS
        .iter()
        .map(|(name, value)| ((*name).to_owned(), Value::String((*value).to_owned())))
        .collect::<Map<_, _>>();
    if comparison["rust_required_headers"] != Value::Object(hardened.clone())
        || expected_headers != Value::Object(hardened)
    {
        return Err(format!(
            "{workflow} / {label}: hardened 503 header contract changed"
        ));
    }
    Ok(expected_headers)
}

fn strip_api_prefix(path: &str) -> &str {
    path.strip_prefix("/settlement/api/v1").unwrap_or(path)
}

fn mutation_signature(method: &str, path: &str, mut payload: Value) -> Option<Value> {
    fn defaults(payload: &Value, fields: &[(&str, Value)]) -> Map<String, Value> {
        let mut output = Map::new();
        for (key, value) in fields {
            output.insert((*key).to_owned(), value.clone());
        }
        if let Some(object) = payload.as_object() {
            output.extend(object.clone());
        }
        output
    }
    let action;
    if path == "/cycles" || is_integer_path(path, "/cycles/") {
        let mut data = defaults(
            &payload,
            &[
                ("start", Value::Null),
                ("start_mode", json!("exact")),
                (
                    "cost_kind",
                    if method == "PATCH" {
                        Value::Null
                    } else {
                        json!("exact")
                    },
                ),
            ],
        );
        if let Some(start) = data.get_mut("start") {
            *start = wire_datetime(start.clone());
        }
        payload = Value::Object(data);
        action = if method == "POST" {
            "create".to_owned()
        } else {
            format!("edit:{}", path.rsplit('/').next().unwrap_or_default())
        };
    } else if path.ends_with("/start-confirm") {
        let mut data = defaults(
            &payload,
            &[
                ("method", json!("legacy")),
                ("source_usage_id", Value::Null),
            ],
        );
        if let Some(start) = data.get_mut("start") {
            *start = wire_datetime(start.clone());
        }
        payload = Value::Object(data);
        action = format!(
            "start-confirm:{}",
            path.split('/').nth(2).unwrap_or_default()
        );
    } else if path.ends_with("/free") {
        payload = Value::Object(defaults(&payload, &[("version", Value::Null)]));
        action = format!("free:{}", path.split('/').nth(2).unwrap_or_default());
    } else if path.ends_with("/upgrade-paid") {
        payload = Value::Object(defaults(&payload, &[("cost_kind", json!("exact"))]));
        action = format!(
            "upgrade-paid:{}",
            path.split('/').nth(2).unwrap_or_default()
        );
    } else if path == "/expenses" || is_integer_path(path, "/expenses/") {
        let mut data = defaults(&payload, &[("allocation_mode", json!("equal"))]);
        let members = data
            .get("members")?
            .as_array()?
            .iter()
            .map(|member| Value::Object(defaults(member, &[("percent", Value::Null)])))
            .collect::<Vec<_>>();
        data.insert("members".to_owned(), Value::Array(members));
        payload = Value::Object(data);
        action = if method == "POST" {
            "create-expense".to_owned()
        } else {
            format!(
                "edit-expense:{}",
                path.rsplit('/').next().unwrap_or_default()
            )
        };
    } else if path.ends_with("/bill-confirm") {
        payload = Value::Object(defaults(
            &payload,
            &[
                ("accept_difference", json!(false)),
                ("accept_outside", json!(false)),
            ],
        ));
        action = format!(
            "bill-confirm:{}",
            path.split('/').nth(2).unwrap_or_default()
        );
    } else if path == "/settlements" {
        let mut data = defaults(&payload, &[("renewals", json!([]))]);
        let renewals = data
            .get("renewals")?
            .as_array()?
            .iter()
            .map(|renewal| Value::Object(defaults(renewal, &[("cost_kind", Value::Null)])))
            .collect::<Vec<_>>();
        data.insert("renewals".to_owned(), Value::Array(renewals));
        if data
            .get("delete_cycle_ids")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            data.remove("delete_cycle_ids");
        }
        payload = Value::Object(data);
        action = "settle".to_owned();
    } else if path.ends_with("/void") {
        action = format!("void:{}", path.split('/').nth(2).unwrap_or_default());
        payload = json!({});
    } else if path == "/batches" {
        action = "merge".to_owned();
    } else if path.ends_with("/actions") {
        action = format!("transition:{}", path.split('/').nth(2).unwrap_or_default());
    } else if path == "/netting/payments" {
        action = "netting-payment".to_owned();
    } else if let Some(kind) = deletion_action(path) {
        action = kind;
    } else if path == "/inbox/read" {
        action = "inbox_read".to_owned();
        payload = json!({});
    } else {
        return None;
    }
    Some(json!([action, payload]))
}

fn is_integer_path(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn deletion_action(path: &str) -> Option<String> {
    let pieces = path.split('/').collect::<Vec<_>>();
    if pieces.len() == 4
        && pieces[1] == "account-deletions"
        && pieces[2].bytes().all(|byte| byte.is_ascii_digit())
        && matches!(pieces[3], "retry" | "cancel")
    {
        Some(format!("delete-{}:{}", pieces[3], pieces[2]))
    } else {
        None
    }
}

fn wire_datetime(value: Value) -> Value {
    let Value::String(value) = value else {
        return value;
    };
    let normalized = value.replace(' ', "T");
    if let Ok(value) = DateTime::parse_from_rfc3339(&normalized) {
        let precision = if value.timestamp_subsec_micros() == 0 {
            SecondsFormat::Secs
        } else {
            SecondsFormat::Micros
        };
        return Value::String(value.to_rfc3339_opts(precision, true));
    }
    if let Ok(value) = NaiveDateTime::parse_from_str(&normalized, "%Y-%m-%dT%H:%M:%S%.f") {
        let format = if value.and_utc().timestamp_subsec_micros() == 0 {
            "%Y-%m-%dT%H:%M:%S"
        } else {
            "%Y-%m-%dT%H:%M:%S%.6f"
        };
        return Value::String(value.format(format).to_string());
    }
    if let Some(value) = value.strip_suffix("+00:00") {
        Value::String(format!("{value}Z"))
    } else {
        Value::String(value)
    }
}

fn clock_alias_definition<'a>(clock: &'a Value, alias: &str) -> Option<&'a Value> {
    clock["aliases"]
        .as_array()?
        .iter()
        .find(|definition| definition["alias"].as_str() == Some(alias))
}

fn timestamp_epoch(value: &str) -> Option<f64> {
    let timestamp = DateTime::parse_from_rfc3339(value).ok()?;
    Some(timestamp.timestamp() as f64 + timestamp.timestamp_subsec_nanos() as f64 / 1_000_000_000.0)
}

fn shift_datetime(value: &str, offset_microseconds: i64) -> Option<String> {
    let timestamp = DateTime::parse_from_rfc3339(value).ok()?;
    let shifted =
        timestamp.checked_add_signed(chrono::Duration::microseconds(offset_microseconds))?;
    Some(shifted.to_rfc3339_opts(SecondsFormat::Micros, false))
}

fn runtime_clock_value_matches(
    clock: &Value,
    alias: &str,
    actual: &Value,
    started_at_epoch: f64,
    now_epoch: f64,
) -> TestResult<bool> {
    let Some(definition) = clock_alias_definition(clock, alias) else {
        return Ok(false);
    };
    let kind = definition["kind"].as_str().unwrap_or("");
    let timestamp = actual
        .as_f64()
        .or_else(|| actual.as_str().and_then(timestamp_epoch));
    let Some(timestamp) = timestamp else {
        return Ok(false);
    };
    let (offset, upper_slack) = match kind {
        "generated_timestamp" | "response_timestamp" => (0.0, 10.0),
        "preview_expiry" => (
            definition["offset_seconds"]
                .as_f64()
                .ok_or_else(|| format!("clock alias {alias} lacks an offset_seconds value"))?,
            10.0,
        ),
        "session_expiry" => (
            definition["max_age_seconds"]
                .as_f64()
                .ok_or_else(|| format!("clock alias {alias} lacks a max_age_seconds value"))?,
            10.0,
        ),
        _ => return Ok(false),
    };
    Ok(timestamp >= started_at_epoch + offset && timestamp <= now_epoch + offset + upper_slack)
}

fn collect_response_clock_bindings(
    clock: &Value,
    expected: &Value,
    actual: &Value,
    bindings: &mut Vec<(String, String)>,
    workflow: &str,
    label: &str,
) -> TestResult<()> {
    if let Some(alias) = expected.as_str()
        && clock_alias_definition(clock, alias).and_then(|definition| definition["kind"].as_str())
            == Some("response_timestamp")
    {
        let value = actual.as_str().ok_or_else(|| {
            format!("{workflow} / {label}: response clock alias {alias} is not a string")
        })?;
        bindings.push((alias.to_owned(), value.to_owned()));
        return Ok(());
    }
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            for (key, expected_value) in expected {
                if let Some(actual_value) = actual.get(key) {
                    collect_response_clock_bindings(
                        clock,
                        expected_value,
                        actual_value,
                        bindings,
                        workflow,
                        label,
                    )?;
                }
            }
        }
        (Value::Array(expected), Value::Array(actual)) => {
            for (expected_value, actual_value) in expected.iter().zip(actual) {
                collect_response_clock_bindings(
                    clock,
                    expected_value,
                    actual_value,
                    bindings,
                    workflow,
                    label,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn normalize_runtime_timestamps(
    value: &mut Value,
    clock: &Value,
    started_at_epoch: f64,
    workflow: &str,
) -> TestResult<()> {
    match value {
        Value::Object(object) => {
            for (key, value) in object.iter_mut() {
                if key == "expires_at"
                    && runtime_clock_value_matches(
                        clock,
                        "<clock:preview-expiry>",
                        value,
                        started_at_epoch,
                        system_epoch(),
                    )?
                {
                    *value = json!("<clock:preview-expiry>");
                    continue;
                }
                if GENERATED_TIMES.contains(&key.as_str())
                    && runtime_clock_value_matches(
                        clock,
                        "<clock:generated>",
                        value,
                        started_at_epoch,
                        system_epoch(),
                    )?
                {
                    *value = json!("<clock:generated>");
                    continue;
                }
                normalize_runtime_timestamps(value, clock, started_at_epoch, workflow)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize_runtime_timestamps(value, clock, started_at_epoch, workflow)?;
            }
        }
        _ => {}
    }
    let _ = workflow;
    Ok(())
}

struct SnapshotContext<'a> {
    aliases: &'a AliasRegistry,
    session_key: Option<&'a str>,
    signatures: &'a HashMap<(i64, String), SignatureInput>,
    upstream: &'a UpstreamServer,
    workflow: &'a str,
    clock: &'a Value,
    started_at_epoch: f64,
}

async fn database_snapshot(
    path: &Path,
    expected: &Value,
    context: SnapshotContext<'_>,
) -> TestResult<Value> {
    let SnapshotContext {
        aliases,
        session_key,
        signatures,
        upstream,
        workflow,
        clock,
        started_at_epoch,
    } = context;
    let runtime_clock = clock["mode"].as_str() == Some("runtime");
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .map_err(display_error)?;
    let session_cipher = session_key
        .map(|key| {
            Fernet::new(key)
                .ok_or_else(|| format!("{workflow}: invalid synthetic session Fernet key"))
        })
        .transpose()?;
    let mut database = Map::new();
    let mut schema = Map::new();
    for table in TABLES {
        let info = sqlx::query(&format!("PRAGMA table_info(\"{table}\")"))
            .fetch_all(&mut connection)
            .await
            .map_err(display_error)?;
        if info.is_empty() {
            return Err(format!("{workflow}: required table {table} is missing"));
        }
        let schema_rows = info
            .into_iter()
            .map(|row| {
                Ok(json!({
                    "cid": row.try_get::<i64, _>("cid").map_err(display_error)?,
                    "name": row.try_get::<String, _>("name").map_err(display_error)?,
                    "type": row.try_get::<String, _>("type").map_err(display_error)?,
                    "notnull": row.try_get::<i64, _>("notnull").map_err(display_error)?,
                    "dflt_value": row.try_get::<Option<String>, _>("dflt_value").map_err(display_error)?,
                    "pk": row.try_get::<i64, _>("pk").map_err(display_error)?
                }))
            })
            .collect::<TestResult<Vec<_>>>()?;
        schema.insert((*table).to_owned(), Value::Array(schema_rows));

        let rows = sqlx::query(&format!("SELECT * FROM \"{table}\""))
            .fetch_all(&mut connection)
            .await
            .map_err(display_error)?;
        let mut normalized_rows = Vec::with_capacity(rows.len());
        for row in rows {
            let mut object = Map::new();
            for (index, column) in row.columns().iter().enumerate() {
                let raw = row.try_get_raw(index).map_err(display_error)?;
                let value = if raw.is_null() {
                    Value::Null
                } else {
                    match raw.type_info().name() {
                        "INTEGER" => {
                            Value::from(row.try_get::<i64, _>(index).map_err(display_error)?)
                        }
                        "REAL" | "FLOAT" | "DOUBLE" => {
                            let number = row.try_get::<f64, _>(index).map_err(display_error)?;
                            serde_json::Number::from_f64(number)
                                .map(Value::Number)
                                .unwrap_or(Value::Null)
                        }
                        "BLOB" => {
                            let bytes = row.try_get::<Vec<u8>, _>(index).map_err(display_error)?;
                            Value::String(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
                        }
                        _ => {
                            let text = row.try_get::<String, _>(index).map_err(display_error)?;
                            if JSON_COLUMNS.contains(&column.name()) {
                                serde_json::from_str(&text).map_err(|error| {
                                    format!(
                                        "{workflow}: invalid JSON in {table}.{}: {error}",
                                        column.name()
                                    )
                                })?
                            } else {
                                Value::String(text)
                            }
                        }
                    }
                };
                object.insert(column.name().to_owned(), value);
            }
            let mut item = Value::Object(object);
            if *table == "sessions" {
                let object = item
                    .as_object_mut()
                    .expect("query row conversion always creates an object");
                let id = object["id"]
                    .as_str()
                    .ok_or_else(|| format!("{workflow}: session id missing"))?;
                let user_id = object["user_id"]
                    .as_i64()
                    .ok_or_else(|| format!("{workflow}: session user_id missing"))?;
                let token_cipher = object["token"]
                    .as_str()
                    .ok_or_else(|| format!("{workflow}: session token ciphertext missing"))?;
                let cipher = session_cipher.as_ref().ok_or_else(|| {
                    format!("{workflow}: session rows exist without a synthetic key")
                })?;
                let token = cipher
                    .decrypt(token_cipher)
                    .map_err(|_| format!("{workflow}: stored session token failed decryption"))?;
                let token = String::from_utf8(token).map_err(display_error)?;
                if upstream.user_id_for_token(&token).await != Some(user_id) {
                    return Err(format!(
                        "{workflow}: stored session token does not match its verified user"
                    ));
                }
                if let Some(alias) = aliases.reverse.get(id) {
                    object.insert("id".to_owned(), Value::String(alias.clone()));
                }
                object.insert(
                    "token".to_owned(),
                    Value::String(format!("<fernet:fixture-user-{user_id}>")),
                );
                if runtime_clock {
                    let expires = object["expires_at"]
                        .as_f64()
                        .ok_or_else(|| format!("{workflow}: session expiry is not numeric"))?;
                    if !runtime_clock_value_matches(
                        clock,
                        "<clock:session-expiry>",
                        &json!(expires),
                        started_at_epoch,
                        system_epoch(),
                    )? {
                        return Err(format!(
                            "{workflow}: runtime session expiry outside its configured window"
                        ));
                    }
                    object.insert("expires_at".to_owned(), json!("<clock:session-expiry>"));
                }
                if let Some(alias) = aliases
                    .reverse
                    .get(object["csrf"].as_str().unwrap_or_default())
                {
                    object.insert("csrf".to_owned(), Value::String(alias.clone()));
                }
            }
            if *table == "requests" {
                let object = item
                    .as_object_mut()
                    .expect("query row conversion always creates an object");
                let actor = object["actor_id"]
                    .as_i64()
                    .ok_or_else(|| format!("{workflow}: request actor_id missing"))?;
                let key = object["key"]
                    .as_str()
                    .ok_or_else(|| format!("{workflow}: request key missing"))?;
                let stored = object["signature"]
                    .as_str()
                    .ok_or_else(|| format!("{workflow}: request signature missing"))?;
                let signature = signatures.get(&(actor, key.to_owned())).ok_or_else(|| {
                    format!(
                        "{workflow}: unexpected idempotency record for actor {actor} key {key:?}"
                    )
                })?;
                if sha256_hex(canonical_json(&signature.actual).as_bytes()) != stored {
                    return Err(format!(
                        "{workflow}: stored idempotency signature does not hash the actual request"
                    ));
                }
                object.insert(
                    "signature".to_owned(),
                    Value::String(sha256_hex(canonical_json(&signature.normalized).as_bytes())),
                );
            }
            if *table == "meta" {
                let object = item.as_object_mut().expect("meta row is an object");
                let key = object["key"].as_str().unwrap_or_default();
                let value = object["value"].clone();
                if runtime_clock
                    && GENERATED_TIMES.contains(&key)
                    && runtime_clock_value_matches(
                        clock,
                        "<clock:generated>",
                        &value,
                        started_at_epoch,
                        system_epoch(),
                    )?
                {
                    object.insert("value".to_owned(), json!("<clock:generated>"));
                } else {
                    object.insert("value".to_owned(), aliases.normalize_value(&value));
                }
            }
            if runtime_clock {
                normalize_runtime_timestamps(&mut item, clock, started_at_epoch, workflow)?;
            }
            item = aliases.normalize_value(&item);
            normalized_rows.push(item);
        }
        normalized_rows.sort_by_key(canonical_json);
        database.insert((*table).to_owned(), Value::Array(normalized_rows));
    }
    connection.close().await.map_err(display_error)?;
    let expected_tables = expected["database"]
        .as_object()
        .ok_or_else(|| format!("{workflow}: expected database snapshot missing"))?;
    if expected_tables.len() != TABLES.len()
        || TABLES
            .iter()
            .any(|table| !expected_tables.contains_key(*table))
    {
        return Err(format!(
            "{workflow}: frozen database snapshot does not enumerate all legacy tables"
        ));
    }
    Ok(json!({"database":database,"schema":schema}))
}

fn canonical_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

async fn integrity_check(path: &Path) -> TestResult<Value> {
    let mut connection = open_read_only(path).await?;
    let rows = sqlx::query("PRAGMA integrity_check")
        .fetch_all(&mut connection)
        .await
        .map_err(display_error)?;
    let values = rows
        .iter()
        .map(|row| {
            row.try_get::<String, _>(0)
                .map(Value::String)
                .map_err(display_error)
        })
        .collect::<TestResult<Vec<_>>>()?;
    connection.close().await.map_err(display_error)?;
    Ok(if values.len() == 1 {
        values[0].clone()
    } else {
        Value::Array(values)
    })
}

async fn foreign_key_check(path: &Path) -> TestResult<Value> {
    let mut connection = open_read_only(path).await?;
    let rows = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut connection)
        .await
        .map_err(display_error)?;
    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        values.push(json!([
            row.try_get::<String, _>("table").map_err(display_error)?,
            row.try_get::<i64, _>("rowid").map_err(display_error)?,
            row.try_get::<String, _>("parent").map_err(display_error)?,
            row.try_get::<i64, _>("fkid").map_err(display_error)?
        ]));
    }
    connection.close().await.map_err(display_error)?;
    Ok(Value::Array(values))
}

async fn sqlite_sequences(path: &Path) -> TestResult<Value> {
    let mut connection = open_read_only(path).await?;
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='sqlite_sequence'",
    )
    .fetch_optional(&mut connection)
    .await
    .map_err(display_error)?;
    let mut values = Vec::new();
    if exists.is_some() {
        let rows = sqlx::query("SELECT name,seq FROM sqlite_sequence ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .map_err(display_error)?;
        for row in rows {
            values.push(json!([
                row.try_get::<String, _>("name").map_err(display_error)?,
                row.try_get::<i64, _>("seq").map_err(display_error)?
            ]));
        }
    }
    connection.close().await.map_err(display_error)?;
    Ok(Value::Array(values))
}

async fn open_read_only(path: &Path) -> TestResult<SqliteConnection> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));
    SqliteConnection::connect_with(&options)
        .await
        .map_err(display_error)
}

async fn open_writable(path: &Path) -> TestResult<SqliteConnection> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(10));
    SqliteConnection::connect_with(&options)
        .await
        .map_err(display_error)
}
