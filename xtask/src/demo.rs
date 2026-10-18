use crate::Result;
use axum::{
    Json, Router,
    extract::{Path as RoutePath, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get},
};
use chrono::{Duration as Days, Utc};
use clap::Args;
use fs2::FileExt;
use serde_json::{Value, json};
use settlement::{config::Config, http::App, time};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle};
use tokio_util::sync::CancellationToken;

const MARKER: &str = "settlement-rust-browser-qa-v1";
const USERS: [&str; 3] = ["林予 · 管理员", "小舟", "时雨"];
const ACCOUNTS: [&str; 4] = [
    "OpenAI Plus · 工作账号",
    "Grok Super · 共享资源",
    "免费资源 · 日常试用",
    "待配置账号",
];

#[derive(Args, Clone)]
pub struct Options {
    /// Retain a fixture only in an empty/owned directory below the system temp directory.
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 18082)]
    pub port: u16,
    #[arg(long, default_value = "frontend/dist")]
    pub assets: PathBuf,
    #[arg(long)]
    pub netting_fixtures: bool,
    /// Fail the first synthetic upstream DELETE for recovery tests.
    #[arg(long)]
    pub fail_delete_once: bool,
    #[arg(long)]
    pub bill_csv: Option<PathBuf>,
    #[arg(long)]
    pub self_check: bool,
}

pub fn check_environment() -> Result<()> {
    for name in [
        "SOURCE_DATABASE_URL",
        "SOURCE_FIXTURE",
        "SUB2API_VERIFY_URL",
        "PUBLIC_ORIGIN",
        "DATA_DIR",
    ] {
        if std::env::var(name).is_ok_and(|s| !s.is_empty()) {
            return Err(format!("Demo refuses inherited {name}; use an isolated shell").into());
        }
    }
    Ok(())
}

struct Workspace {
    path: PathBuf,
    _temporary: Option<TempDir>,
    _lock: fs::File,
}
impl Workspace {
    fn open(requested: Option<&Path>) -> Result<Self> {
        let temporary = if requested.is_none() {
            Some(tempfile::tempdir()?)
        } else {
            None
        };
        let path = requested
            .or_else(|| temporary.as_ref().map(TempDir::path))
            .unwrap();
        if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err("Demo refuses a symlink data directory".into());
        }
        fs::create_dir_all(path)?;
        let path = fs::canonicalize(path)?;
        let root = fs::canonicalize(std::env::temp_dir())?;
        let unix_temp = fs::canonicalize("/tmp").unwrap_or_else(|_| root.clone());
        if path == root
            || path == unix_temp
            || (!path.starts_with(&root) && !path.starts_with(&unix_temp))
        {
            return Err("Demo data must be in an isolated temporary directory".into());
        }
        let marker = path.join("qa-fixture.json");
        if !marker.exists() && fs::read_dir(&path)?.next().is_some() {
            return Err("Refusing a nonempty directory without a demo ownership marker".into());
        }
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.join("qa-server.lock"))?;
        FileExt::try_lock_exclusive(&lock).map_err(|_| "The demo directory is already in use")?;
        if marker.exists() {
            let state: Value = serde_json::from_slice(&fs::read(&marker)?)?;
            if state["kind"] != MARKER {
                return Err("Invalid demo ownership marker".into());
            }
        } else {
            atomic_json(
                &marker,
                &json!({"kind":MARKER,"clock":time::format(Utc::now()),"base_seeded":false,"netting_seeded":false}),
            )?;
        }
        Ok(Self {
            path,
            _temporary: temporary,
            _lock: lock,
        })
    }
}

pub fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let next = path.with_extension("json.next");
    fs::write(&next, serde_json::to_vec(value)?)?;
    fs::rename(next, path)?;
    Ok(())
}

pub fn source(clock: &str) -> Result<Value> {
    let clock = time::parse(clock)?;
    let ago = |days| time::format(clock - Days::days(days));
    let accounts = ACCOUNTS.iter().enumerate().map(|(index,name)| json!({"id":index+1,"name":name,"platform":if index%2==0{"openai"}else{"grok"},"status":"active","created_at":ago(30),"deleted":false})).collect::<Vec<_>>();
    let users = USERS.iter().enumerate().map(|(index,name)| json!({"id":index+1,"name":name,"role":if index==0{"admin"}else{"user"},"status":"active"})).collect::<Vec<_>>();
    let usage = [(1,1,1,"0.000001",15),(2,1,1,"60",3),(3,1,2,"30",2),(4,1,3,"10",1),(5,2,1,"10",3),(6,2,2,"40",2),(7,2,3,"50",1)].into_iter().map(|(id,account,user,amount,days)| json!({"id":id,"account_id":account,"user_id":user,"amount":amount,"created_at":ago(days),"request_id":format!("qa-{id}"),"model":"qa-model","input_tokens":id*10,"output_tokens":id,"cache_read_tokens":0,"cache_creation_tokens":0})).collect::<Vec<_>>();
    Ok(json!({"accounts":accounts,"users":users,"usage":usage,"cleanups":[]}))
}

#[derive(Clone)]
struct Upstream {
    fixture: PathBuf,
    lock: Arc<Mutex<()>>,
    fail_delete_once: Arc<AtomicBool>,
}
fn actor(headers: &HeaderMap) -> Option<usize> {
    headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer demo-")?
        .parse::<usize>()
        .ok()
        .filter(|id| (1..=3).contains(id))
}
async fn auth(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    match actor(&headers) {
        Some(id) => (
            StatusCode::OK,
            Json(
                json!({"code":0,"data":{"id":id,"username":USERS[id-1],"role":if id==1{"admin"}else{"user"},"status":"active"}}),
            ),
        ),
        None => (StatusCode::UNAUTHORIZED, Json(json!({"code":401}))),
    }
}
async fn remove(
    State(state): State<Upstream>,
    RoutePath(id): RoutePath<i64>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if actor(&headers) != Some(1) {
        return (StatusCode::FORBIDDEN, Json(json!({"code":403})));
    }
    if state.fail_delete_once.swap(false, Ordering::AcqRel) {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"code":503})));
    }
    let _guard = state.lock.lock().await;
    let result: Result<bool> = (|| {
        let mut value: Value = serde_json::from_slice(&fs::read(&state.fixture)?)?;
        let accounts = value["accounts"]
            .as_array_mut()
            .ok_or("Invalid demo accounts")?;
        if !accounts.iter().any(|a| a["id"] == id) {
            return Ok(false);
        }
        let mut ids = std::collections::BTreeSet::from([id]);
        loop {
            let before = ids.len();
            for a in accounts.iter() {
                if a["parent_account_id"]
                    .as_i64()
                    .is_some_and(|v| ids.contains(&v))
                {
                    ids.insert(a["id"].as_i64().ok_or("Invalid demo account")?);
                }
            }
            if before == ids.len() {
                break;
            }
        }
        for a in accounts {
            if a["id"].as_i64().is_some_and(|v| ids.contains(&v)) {
                a["deleted"] = json!(true);
            }
        }
        atomic_json(&state.fixture, &value)?;
        Ok(true)
    })();
    match result {
        Ok(true) => (StatusCode::OK, Json(json!({"code":0,"data":{}}))),
        Ok(false) => (StatusCode::NOT_FOUND, Json(json!({"code":404}))),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"code":503}))),
    }
}

pub struct Api {
    pub origin: String,
    client: reqwest::Client,
    cookie: String,
    csrf: String,
    uid: i64,
}
impl Api {
    pub async fn login(origin: &str, uid: i64) -> Result<Self> {
        Self::login_with_timeout(origin, uid, Duration::from_secs(60)).await
    }
    pub async fn login_with_timeout(origin: &str, uid: i64, timeout: Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()?;
        let response = client
            .get(format!(
                "{origin}/settlement/sso/start?token=demo-{uid}&user_id={uid}&theme=light"
            ))
            .send()
            .await?;
        if response.status() != 303 {
            return Err("Demo SSO failed".into());
        }
        let cookie = response
            .headers()
            .get("set-cookie")
            .ok_or("Missing demo cookie")?
            .to_str()?
            .split(';')
            .next()
            .unwrap_or("")
            .to_owned();
        let mut api = Self {
            origin: origin.into(),
            client,
            cookie,
            csrf: String::new(),
            uid,
        };
        let actor = api.request("GET", "/me", Value::Null, "login").await?;
        api.csrf = actor["csrf"].as_str().ok_or("Missing CSRF")?.into();
        Ok(api)
    }
    pub async fn request(&self, method: &str, path: &str, body: Value, key: &str) -> Result<Value> {
        let mut request = self
            .client
            .request(
                method.parse()?,
                format!("{}/settlement/api/v1{path}", self.origin),
            )
            .header("Cookie", &self.cookie)
            .header("X-Settlement-User", self.uid.to_string());
        if method != "GET" {
            request = request
                .header("Origin", &self.origin)
                .header("X-CSRF-Token", &self.csrf)
                .header("Idempotency-Key", key)
                .json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let value: Value = response.json().await?;
        if status != 200 {
            return Err(format!(
                "Synthetic API {method} {} failed: {status}: {value}",
                path.split('?').next().unwrap_or("")
            )
            .into());
        }
        Ok(value)
    }
}

pub struct Running {
    pub app: App,
    pub origin: String,
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    _workspace: Workspace,
    _assets: TempDir,
    _guard: CancelGuard,
}
struct CancelGuard(CancellationToken);
impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
impl Running {
    pub async fn close(self) {
        self.cancel.cancel();
        for task in self.tasks {
            let _ = task.await;
        }
        self.app.service.source.close().await;
        self.app.service.store.close().await;
    }
}

fn copy_assets(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    if source.is_dir() {
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                copy_assets(&entry.path(), &target.join(entry.file_name()))?;
            } else if entry.file_type()?.is_file() {
                fs::copy(entry.path(), target.join(entry.file_name()))?;
            }
        }
    }
    Ok(())
}

pub async fn start(options: &Options, override_source: Option<Value>) -> Result<Running> {
    check_environment()?;
    let workspace = Workspace::open(options.data_dir.as_deref())?;
    let marker = workspace.path.join("qa-fixture.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&marker)?)?;
    let clock = state["clock"]
        .as_str()
        .ok_or("Missing fixture clock")?
        .to_owned();
    let fixture = workspace.path.join("source.json");
    if !fixture.exists() {
        if state["base_seeded"] == true {
            return Err("Saved source fixture is missing".into());
        }
        atomic_json(&fixture, &override_source.unwrap_or(source(&clock)?))?;
    } else if override_source.is_some() {
        return Err("Refusing to replace saved demo data".into());
    }
    let assets = tempfile::tempdir()?;
    copy_assets(&options.assets, assets.path())?;
    let qa = assets.path().join("assets/__qa__");
    fs::create_dir_all(&qa)?;
    fs::write(qa.join("index.html"), include_str!("demo.html"))?;
    fs::write(qa.join("entry.js"), include_str!("demo.js"))?;
    let listener = TcpListener::bind(("127.0.0.1", options.port)).await?;
    let bind = listener.local_addr()?;
    let origin = format!("http://{bind}");
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await?;
    let verify = format!("http://{}", upstream.local_addr()?);
    let cancel = CancellationToken::new();
    let guard = CancelGuard(cancel.clone());
    let stop = cancel.clone();
    let upstream_router = Router::new()
        .route("/api/v1/auth/me", get(auth))
        .route("/api/v1/admin/accounts/{id}", delete(remove))
        .with_state(Upstream {
            fixture: fixture.clone(),
            lock: Arc::new(Mutex::new(())),
            fail_delete_once: Arc::new(AtomicBool::new(options.fail_delete_once)),
        });
    let mut tasks = vec![tokio::spawn(async move {
        let _ = axum::serve(upstream, upstream_router)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await;
    })];
    let app = App::new(Config {
        data_dir: workspace.path.clone(),
        source_url: String::new(),
        fixture: Some(fixture.clone()),
        origin: origin.clone(),
        verify_url: verify,
        bind,
        background: true,
        read_only: false,
        external_writes: true,
        test: true,
        revision: "demo-fixture".into(),
        assets: assets.path().into(),
    })
    .await?;
    app.service.sync(true).await?;
    app.recover_deletions().await?;
    let stop = cancel.clone();
    let router = app.router();
    tasks.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await;
    }));
    let worker = app.clone();
    let stop = cancel.clone();
    tasks.push(tokio::spawn(async move {
        loop {
            tokio::select! { _=stop.cancelled()=>break, _=worker.deletion_wake.notified()=>{}, _=tokio::time::sleep(Duration::from_secs(5))=>{} }
            let _=worker.service.inventory.refresh(false).await;
            let _=worker.pending_deletions().await;
            let _=worker.service.sync(false).await;
        }
    }));
    let running = Running {
        app,
        origin: origin.clone(),
        cancel,
        tasks,
        _workspace: workspace,
        _assets: assets,
        _guard: guard,
    };
    let setup: Result<()> = async {
        let api = Api::login(&origin,1).await?;
        if state["base_seeded"] != true {
            let start = time::format(time::parse(&clock)? - Days::days(7));
            for (id,cost) in [(1,"100.00"),(2,"240.00")] {
                api.request("POST","/cycles",json!({"account_id":id,"name":ACCOUNTS[id-1],"cost":cost,"creditor_id":1,"start":start,"start_mode":if id==2{"minute_estimate"}else{"exact"},"cost_kind":"exact"}),&format!("qa-base-cycle-{id}")).await?;
            }
            api.request("PUT","/accounts/3/free",json!({"name":ACCOUNTS[2],"version":null}),"qa-base-free").await?;
            api.request("POST","/expenses",json!({"name":"固定费用 · 服务订阅","cost":"30.00","incurred_on":&clock[..10],"creditor_id":1,"allocation_mode":"equal","members":[{"user_id":1,"percent":null},{"user_id":2,"percent":null},{"user_id":3,"percent":null}]}),"qa-open-expense").await?;
            state["base_seeded"]=json!(true);
            atomic_json(&marker,&state)?;
        }
        if options.netting_fixtures && state["netting_seeded"] != true {
            for (index,(cost,creditor,payer)) in [("100.00",2,1),("60.00",1,2),("20.00",3,1),("20.00",1,3),("5.00",3,2)].into_iter().enumerate() {
                let expense = api.request("POST","/expenses",json!({"name":format!("演示账单 {}",index+1),"cost":cost,"incurred_on":&clock[..10],"creditor_id":creditor,"allocation_mode":"equal","members":[{"user_id":payer,"percent":null}]}),&format!("qa-netting-expense-{index}")).await?;
                // The process may have stopped after settlement committed but before
                // the seed marker was saved. The ledger is the durable authority.
                let settled: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM settlements WHERE expense_id=? AND status='active')")
                    .bind(expense["id"].as_i64().ok_or("Missing demo expense ID")?)
                    .fetch_one(&running.app.service.store.reads).await?;
                if settled { continue; }
                let preview = api.request("POST","/previews",json!({"ids":[],"expense_ids":[expense["id"]],"end":time::now()}),"qa-preview").await?;
                api.request("POST","/settlements",json!({"preview_id":preview["id"],"renewals":[]}),&format!("qa-netting-settle-{index}")).await?;
            }
            state["netting_seeded"]=json!(true);
            atomic_json(&marker,&state)?;
        }
        if let Some(path)=&options.bill_csv {
            let data:Value=serde_json::from_slice(&fs::read(&fixture)?)?;
            write_bill(path,&data,&clock)?;
        }
        Ok(())
    }.await;
    if let Err(error) = setup {
        running.close().await;
        return Err(error);
    }
    Ok(running)
}

fn write_bill(path: &Path, source: &Value, clock: &str) -> Result<()> {
    let mut writer = csv::Writer::from_writer(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?,
    );
    writer.write_record([
        "Time",
        "Billed Cost",
        "Model",
        "Input Tokens",
        "Output Tokens",
        "Cache Read Tokens",
        "Cache Creation Tokens",
        "Request ID",
    ])?;
    for row in source["usage"].as_array().ok_or("Invalid fixture")? {
        if row["account_id"] == 2 {
            writer.write_record([
                row["created_at"].as_str().unwrap_or("").into(),
                row["amount"].as_str().unwrap_or("").into(),
                row["model"].as_str().unwrap_or("").into(),
                row["input_tokens"].to_string(),
                row["output_tokens"].to_string(),
                "0".into(),
                "0".into(),
                row["request_id"].as_str().unwrap_or("").into(),
            ])?;
        }
    }
    writer.write_record([
        time::format(time::parse(clock)? - Days::days(1)),
        "50.00264773".into(),
        "unmatched".into(),
        "99".into(),
        "99".into(),
        "0".into(),
        "0".into(),
        "missing".into(),
    ])?;
    writer.flush()?;
    Ok(())
}

pub async fn run(options: Options) -> Result<()> {
    if !options.self_check && !options.assets.join("index.html").is_file() {
        return Err("Build the frontend before opening the demo".into());
    }
    let running = start(&options, None).await?;
    let result: Result<()> = async {
        let api = Api::login(&running.origin, 1).await?;
        let overview = api.request("GET", "/overview", Value::Null, "").await?;
        if !overview["sync_error"].is_null() || !overview["account_sync_error"].is_null() {
            return Err("Demo synchronization failed".into());
        }
        for uid in [2, 3] {
            Api::login(&running.origin, uid).await?;
        }
        if options.self_check {
            println!("demo self-check: passed; synthetic users=3");
        } else {
            println!("{}/settlement/assets/__qa__/index.html", running.origin);
            shutdown().await?;
        }
        Ok(())
    }
    .await;
    running.close().await;
    result
}
async fn shutdown() -> Result<()> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! { result=tokio::signal::ctrl_c()=>result?, _=term.recv()=>{} }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_refuses_unowned_data_and_parallel_owners() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("existing"), "preserve").unwrap();
        assert!(Workspace::open(Some(temporary.path())).is_err());
        let owned = Workspace::open(None).unwrap();
        assert!(Workspace::open(Some(&owned.path)).is_err());
        assert_eq!(
            fs::read_to_string(temporary.path().join("existing")).unwrap(),
            "preserve"
        );
    }
    #[tokio::test]
    async fn demo_runs_rust_and_restarts_with_the_same_synthetic_ledger() {
        let temp = tempfile::tempdir().unwrap();
        let options = Options {
            data_dir: Some(temp.path().into()),
            port: 0,
            assets: PathBuf::from("missing-assets"),
            netting_fixtures: true,
            fail_delete_once: false,
            bill_csv: None,
            self_check: true,
        };
        let server = start(&options, None).await.unwrap();
        let api = Api::login(&server.origin, 1).await.unwrap();
        let balance = api
            .request("GET", "/netting/balances/1/2", Value::Null, "")
            .await
            .unwrap();
        assert_eq!(balance["amount_fen"], 4000);
        assert_eq!(balance["offset_fen"], 6000);
        let rows = api
            .request("GET", "/orders?scope=all", Value::Null, "")
            .await
            .unwrap();
        assert_eq!(rows["orders"].as_array().unwrap().len(), 5);
        server.close().await;
        let marker = temp.path().join("qa-fixture.json");
        let mut state: Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
        state["netting_seeded"] = json!(false);
        atomic_json(&marker, &state).unwrap();
        let server = start(&options, None).await.unwrap();
        let api = Api::login(&server.origin, 1).await.unwrap();
        let after = api
            .request("GET", "/orders?scope=all", Value::Null, "")
            .await
            .unwrap();
        assert_eq!(after, rows);
        server.close().await;
    }
}
