use crate::{
    config::Config,
    error::{Error, Result},
    json::{array, hash, int, text},
    service::{Prepared, Service, admin},
    source::Source,
    store::{self, Store},
    time,
};
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{any, get},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, Notify, Semaphore};
#[derive(Clone)]
pub struct App {
    pub config: Config,
    pub service: Service,
    pub client: reqwest::Client,
    pub cipher: Arc<crate::keys::Keyring>,
    pub limiter: Arc<crate::rate::Limiter>,
    pub metrics: Arc<crate::metrics::Metrics>,
    pub reminders: Arc<crate::reminders::Reminders>,
    _ledger_lock: Option<Arc<crate::keys::LedgerLock>>,
    pub deletion_lock: Arc<Mutex<()>>,
    pub deletion_wake: Arc<Notify>,
    pub requests: Arc<Semaphore>,
}
impl App {
    pub async fn new(config: Config) -> Result<Self> {
        let ledger_lock = if config.test {
            None
        } else {
            Some(Arc::new(crate::keys::LedgerLock::acquire(
                &config.data_dir,
            )?))
        };
        let store = Store::open(&config.data_dir.join("settlement.sqlite3")).await?;
        let cipher = crate::keys::Keyring::load(&config.data_dir, &store).await?;
        let limiter = crate::rate::Limiter::environment(config.test)?;
        let metrics = crate::metrics::Metrics::environment(config.test)?;
        let reminders = crate::reminders::Reminders::environment(config.test, config.read_only)?;
        let source = Source::new(&config).await?;
        let service = Service::new(store, source).await?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(6))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::unavailable())?;
        Ok(Self {
            config,
            service,
            client,
            cipher: Arc::new(cipher),
            limiter: Arc::new(limiter),
            metrics: Arc::new(metrics),
            reminders: Arc::new(reminders),
            _ledger_lock: ledger_lock,
            deletion_lock: Arc::new(Mutex::new(())),
            deletion_wake: Arc::new(Notify::new()),
            requests: Arc::new(Semaphore::new(64)),
        })
    }
    pub fn decrypt(&self, encrypted: &str) -> Result<String> {
        String::from_utf8(self.cipher.decrypt(encrypted)?)
            .map_err(|_| Error::new(401, "登录已失效"))
    }
    pub async fn validate(&self, token: &str) -> Result<Value> {
        if token.is_empty() || token.len() > 16384 {
            return Err(Error::new(401, "请从 Sub2API 重新进入结算中心"));
        }
        let started = std::time::Instant::now();
        let response = self
            .client
            .get(format!("{}/api/v1/auth/me", self.config.verify_url))
            .bearer_auth(token)
            .send()
            .await;
        tracing::debug!(
            task = "upstream_auth",
            elapsed_ms = started.elapsed().as_millis() as u64
        );
        let response = response.map_err(|_| Error::new(503, "登录校验暂不可用"))?;
        if [401, 403].contains(&response.status().as_u16()) {
            return Err(Error::new(401, "登录已失效，请从 Sub2API 重新进入"));
        }
        if response.status() != 200 {
            return Err(Error::new(503, "登录校验暂不可用"));
        }
        let body = response
            .json::<Value>()
            .await
            .map_err(|_| Error::new(401, "登录身份无效"))?;
        let u = &body["data"];
        let id = u["id"]
            .as_i64()
            .or_else(|| u["id"].as_str().and_then(|s| s.parse().ok()))
            .filter(|i| *i > 0)
            .ok_or_else(|| Error::new(401, "登录身份无效"))?;
        if body["code"] != 0
            || !["admin", "user"].contains(&text(u, "role"))
            || u.get("status").is_some_and(|s| s != "active")
        {
            return Err(Error::new(401, "登录身份无效"));
        }
        Ok(
            json!({"id":id,"name":u["username"].as_str().filter(|s|!s.is_empty()).map(str::to_owned).unwrap_or_else(||format!("用户 #{id}")),"role":u["role"]}),
        )
    }
    async fn authenticate(&self, headers: &HeaderMap, write: bool) -> Result<(Value, String)> {
        let raw = cookie(headers);
        let sid = hash(raw.as_bytes());
        let mut db = self.service.store.read().await?;
        let session = store::optional(&mut db, "SELECT * FROM sessions WHERE id=?", &[json!(sid)])
            .await?
            .filter(|s| !time::is_expired(&s["expires_at"]))
            .ok_or_else(|| Error::new(401, "请从 Sub2API 进入结算中心"))?;
        drop(db);
        let token = self.decrypt(text(&session, "token"))?;
        let mut actor = self.validate(&token).await?;
        if actor["id"] != session["user_id"] {
            return Err(Error::new(401, "登录身份已改变"));
        }
        if let Some(expected) = headers.get("X-Settlement-User")
            && expected.as_bytes() != int(&actor, "id").to_string().as_bytes()
        {
            return Err(Error::new(401, "登录账号已切换，请从 Sub2API 重新进入"));
        }
        if write
            && (header(headers, "Origin") != self.config.origin
                || !bool::from(
                    header(headers, "X-CSRF-Token")
                        .as_bytes()
                        .ct_eq(text(&session, "csrf").as_bytes()),
                ))
        {
            return Err(Error::new(403, "请求校验失败，请重新进入"));
        }
        actor["csrf"] = session["csrf"].clone();
        Ok((actor, sid))
    }
    async fn sso(
        &self,
        headers: &HeaderMap,
        params: &BTreeMap<String, String>,
    ) -> Result<Response> {
        let token = params.get("token").map(String::as_str).unwrap_or("");
        let actor = self.validate(token).await?;
        if params.get("user_id").and_then(|s| s.parse::<i64>().ok()) != Some(int(&actor, "id")) {
            return Err(Error::new(403, "登录用户不匹配"));
        }
        let mut expires = time::epoch() + 8.0 * 3600.0;
        // validate() has already verified this token through Sub2API. Decoding
        // the payload here does not authenticate it: exp can only shorten the
        // local eight-hour session, never extend it or bypass upstream checks.
        if let Some(claims) = token
            .split('.')
            .nth(1)
            .and_then(|s| URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).ok())
            .and_then(|s| serde_json::from_slice::<Value>(&s).ok())
            && let Some(exp) = claims["exp"]
                .as_f64()
                .filter(|exp| exp.is_finite() && *exp >= 0.0)
        {
            expires = expires.min(exp);
        }
        let mut db = self.service.store.write().await?;
        store::exec(
            &mut db,
            "DELETE FROM sessions WHERE expires_at<=?",
            &[json!(time::epoch())],
        )
        .await?;
        let mut raw = cookie(headers).to_owned();
        let sid = hash(raw.as_bytes());
        let existing = store::optional(
            &mut db,
            "SELECT * FROM sessions WHERE id=? AND user_id=?",
            &[json!(sid), actor["id"].clone()],
        )
        .await?;
        let encrypted = self.cipher.encrypt(token.as_bytes());
        if existing.is_some() {
            store::exec(
                &mut db,
                "UPDATE sessions SET token=?,expires_at=? WHERE id=?",
                &[json!(encrypted), json!(expires), json!(sid)],
            )
            .await?;
        } else {
            raw = random();
            let csrf = random();
            store::exec(
                &mut db,
                "INSERT INTO sessions VALUES(?,?,?,?,?)",
                &[
                    json!(hash(raw.as_bytes())),
                    actor["id"].clone(),
                    json!(encrypted),
                    json!(csrf),
                    json!(expires),
                ],
            )
            .await?;
        }
        store::exec(&mut db,"INSERT INTO rust_session_keys(session_id,key_id) VALUES(?,?) ON CONFLICT(session_id) DO UPDATE SET key_id=excluded.key_id",&[json!(hash(raw.as_bytes())),json!(self.cipher.active)]).await?;
        db.commit().await?;
        let target = if params.get("theme").is_some_and(|s| s == "dark") {
            "/settlement/?theme=dark"
        } else {
            "/settlement/?theme=light"
        };
        let mut response = Redirect::to(target).into_response();
        let cookie = format!(
            "settlement_session={raw}; HttpOnly; Max-Age={}; Path=/settlement/; SameSite=lax{}",
            ((expires - time::epoch()) as i64).max(1),
            if self.config.origin.starts_with("https://") {
                "; Secure"
            } else {
                ""
            }
        );
        response.headers_mut().insert(
            "Set-Cookie",
            HeaderValue::from_str(&cookie).map_err(|_| Error::unavailable())?,
        );
        Ok(response)
    }
    pub fn router(&self) -> Router {
        Router::new()
            .route("/metrics", get(crate::metrics::endpoint))
            .route("/settlement/healthz", get(health))
            .route("/settlement/sso/start", get(sso))
            .route("/settlement/api/v1/{*path}", any(api))
            .route("/settlement/", get(index))
            .nest_service(
                "/settlement/assets",
                tower_http::services::ServeDir::new(self.config.assets.join("assets")),
            )
            .layer(middleware::from_fn(security))
            .with_state(self.clone())
    }
    async fn dispatch(
        &self,
        method: &str,
        route: Route<'_>,
        query: &BTreeMap<String, String>,
        headers: &HeaderMap,
        body: Value,
        identity: (&Value, &str),
    ) -> Result<Value> {
        let (actor, sid) = identity;
        let s = &self.service;
        let scope = query.get("scope").map(String::as_str).unwrap_or("mine");
        let query_fields: &[(&str, &[&str])] = match route {
            Route::NettingPayments if method == "GET" => &[("scope", &["mine", "all"])],
            Route::NettingBalances => &[("scope", &["mine", "receivable", "all"])],
            Route::Orders => &[
                ("scope", &["mine", "receivable", "all"]),
                ("view", &["full"]),
            ],
            _ => &[],
        };
        crate::input::query_choices(query, query_fields)?;
        if method == "GET" {
            return match route {
                Route::Me => Ok(crate::json::merge(
                    actor.clone(),
                    json!({"version": crate::VERSION}),
                )),
                Route::Vendors => s.vendors(actor).await,
                Route::Audit => s.audit_log(actor, query, false).await,
                Route::OperationsStatus => s.operation_status(actor).await,
                Route::Overview => s.overview(actor).await,
                Route::Catalog => s.catalog(actor).await,
                Route::AccountsState => s.account_state(actor).await,
                Route::DeletionScope(aid) => s.deletion_scope(actor, path_id(aid)?).await,
                Route::AccountDeletions => self.deletion_list(actor, None).await,
                Route::StartCandidate(cid) => {
                    s.start_candidate(
                        actor,
                        path_id(cid)?,
                        query.get("after").map(String::as_str).unwrap_or(""),
                    )
                    .await
                }
                Route::Cycle(cid) => s.cycle_detail(actor, path_id(cid)?).await,
                Route::Expenses => s.expenses(actor, None).await,
                Route::Expense(eid) => s.expenses(actor, Some(path_id(eid)?)).await,
                Route::NettingBalances => {
                    let mut db = s.store.read().await?;
                    Ok(json!({"balances":Service::balances(&mut db,actor,scope).await?}))
                }
                Route::NettingPair(a, b) => {
                    s.netting_balance(actor, path_id(a)?, path_id(b)?).await
                }
                Route::NettingPayments => s.netting_payments(actor, scope, None).await,
                Route::NettingPayment(pid) => {
                    s.netting_payments(actor, "mine", Some(path_id(pid)?)).await
                }
                Route::Orders => s.orders(actor, scope).await,
                Route::Order(oid) => s.detail(actor, path_id(oid)?).await,
                Route::History => s.history(actor).await,
                Route::Inbox => s.inbox(actor).await,
                _ => Err(Error::new(404, "Not Found")),
            };
        }
        let (kind, entity) = match route {
            Route::Vendors => ("create-vendor", None),
            Route::VendorMembers(vid) => ("add-vendor-members", Some(path_id(vid)?)),
            Route::VendorRenew(vid) => ("renew-vendor", Some(path_id(vid)?)),
            Route::VendorArchive(vid) => ("archive-vendor", Some(path_id(vid)?)),
            Route::VendorPeriod(pid) => ("edit-vendor-period", Some(path_id(pid)?)),
            Route::VendorStartConfirm(pid) => ("vendor-start-confirm", Some(path_id(pid)?)),
            Route::VendorBillPreviews(pid) => {
                let payload = crate::input::normalize("vendor-bill-preview", body)?;
                return s.vendor_bill_preview(actor, path_id(pid)?, &payload).await;
            }
            Route::VendorBillConfirm(pid) => ("vendor-bill-confirm", Some(path_id(pid)?)),
            Route::VendorPeriodVoid(pid) => ("void-vendor", Some(path_id(pid)?)),
            Route::Cycles => ("create", None),
            Route::Cycle(cid) => ("edit", Some(path_id(cid)?)),
            Route::CycleStartConfirm(cid) => ("start-confirm", Some(path_id(cid)?)),
            Route::AccountFree(aid) => ("free", Some(path_id(aid)?)),
            Route::AccountUpgradePaid(aid) => ("upgrade-paid", Some(path_id(aid)?)),
            Route::CycleBillPreviews(cid) => {
                let payload = crate::input::normalize("bill-preview", body)?;
                return s.bill_preview(actor, path_id(cid)?, &payload).await;
            }
            Route::CycleBillConfirm(cid) => ("bill-confirm", Some(path_id(cid)?)),
            Route::Previews => {
                let payload = crate::input::normalize("preview", body)?;
                return s.preview(actor, &payload).await;
            }
            Route::Expenses => ("create-expense", None),
            Route::Expense(eid) => ("edit-expense", Some(path_id(eid)?)),
            Route::Settlements => ("settle", None),
            Route::SettlementVoid(sid) => ("void", Some(path_id(sid)?)),
            Route::NettingPayments => ("netting-payment", None),
            Route::OrderActions(oid) => ("transition", Some(path_id(oid)?)),
            Route::InboxRead => ("inbox_read", None),
            Route::DeletionRetry(jid) => ("delete-retry", Some(path_id(jid)?)),
            Route::DeletionCancel(jid) => ("delete-cancel", Some(path_id(jid)?)),
            _ => return Err(Error::new(404, "Not Found")),
        };
        if ["settle", "bill-confirm", "start-confirm", "delete-retry"].contains(&kind)
            || kind.contains("vendor")
        {
            admin(actor)?;
        }
        let payload = crate::input::normalize(kind, body)?;
        let action = entity
            .map(|id| format!("{kind}:{id}"))
            .unwrap_or_else(|| kind.to_owned());
        let key = header(headers, "Idempotency-Key");
        let replay = s.replay(actor, key, &action, &payload).await?;
        let mut result = if let Some(value) = replay {
            value
        } else {
            let mut prepared = Prepared::default();
            match kind {
                "vendor-bill-confirm" => {
                    prepared.vendor_bill = Some(
                        s.prepare_vendor_bill(actor, required_entity(entity)?, &payload)
                            .await?,
                    );
                }
                "settle" => {
                    prepared = s.prepare_settlement(actor, &payload).await?;
                }
                "bill-confirm" => {
                    prepared.bill = Some(
                        s.prepare_bill(actor, required_entity(entity)?, &payload)
                            .await?,
                    );
                }
                "start-confirm" => {
                    s.sync_selection(
                        &[json!(required_entity(entity)?)],
                        &time::now(),
                        payload["start"]
                            .as_str()
                            .map(crate::time::instant)
                            .transpose()?
                            .as_deref(),
                    )
                    .await?;
                }
                "delete-retry"
                    if payload.get("scope_digest").is_some()
                        && !s.inventory.refresh(true).await["error"].is_null() =>
                {
                    return Err(Error::new(503, "无法核验删除范围，请稍后重试"));
                }
                _ => {}
            }
            s.mutate(actor, key, &action, &payload, prepared, Some(sid))
                .await?
        };
        if kind == "settle" && !array(&result["deletions"]).is_empty() {
            let ids = array(&result["deletions"])
                .iter()
                .map(|j| int(j, "id"))
                .collect::<Vec<_>>();
            result["deletions"] = self.deletion_list(actor, Some(&ids)).await?["items"].clone();
            self.deletion_wake.notify_one();
        }
        if kind == "delete-retry" {
            let items = self
                .deletion_list(actor, Some(&[required_entity(entity)?]))
                .await?;
            // Capture the response before waking the worker, as settlement does above.
            self.deletion_wake.notify_one();
            return Ok(items["items"][0].clone());
        }
        Ok(result)
    }
}
fn path_id(raw: &str) -> Result<i64> {
    raw.parse::<i64>()
        .map_err(|_| Error::invalid("路径编号无效"))
}
fn required_entity(entity: Option<i64>) -> Result<i64> {
    entity.ok_or_else(|| Error::new(400, "缺少记录编号"))
}
fn header<'a>(h: &'a HeaderMap, k: &str) -> &'a str {
    h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("")
}
fn cookie(h: &HeaderMap) -> &str {
    header(h, "Cookie")
        .split(';')
        .find_map(|s| s.trim().strip_prefix("settlement_session="))
        .unwrap_or("")
}
fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
fn params(request: &Request) -> BTreeMap<String, String> {
    request
        .uri()
        .query()
        .and_then(|s| serde_urlencoded::from_str(s).ok())
        .unwrap_or_default()
}
async fn sso(State(app): State<App>, request: Request) -> Result<Response> {
    app.limiter.sso(app.limiter.request_ip(&request)).await?;
    app.sso(request.headers(), &params(&request)).await
}
async fn index(State(app): State<App>) -> Result<Response> {
    let html = tokio::fs::read(app.config.assets.join("index.html"))
        .await
        .map_err(|_| Error::new(503, "界面尚未构建"))?;
    Ok(([("Content-Type", "text/html; charset=utf-8")], html).into_response())
}
async fn health(State(app): State<App>) -> Result<Response> {
    let mut db = app.service.store.read().await?;
    let synced = store::meta(&mut db, "last_sync").await?;
    let error = store::meta(&mut db, "sync_error").await?;
    let ok = !synced.is_null() && error.is_null();
    Ok((if ok{StatusCode::OK}else{StatusCode::SERVICE_UNAVAILABLE},Json(json!({"status":if ok{"ok"}else{"degraded"},"version":crate::VERSION,"revision":app.config.revision,"engine":"rust"}))).into_response())
}
#[derive(Clone, Copy)]
enum Route<'a> {
    Me,
    Overview,
    Catalog,
    History,
    Inbox,
    InboxRead,
    Audit,
    AuditCsv,
    OperationsStatus,
    AccountsState,
    AccountFree(&'a str),
    AccountUpgradePaid(&'a str),
    DeletionScope(&'a str),
    AccountDeletions,
    DeletionRetry(&'a str),
    DeletionCancel(&'a str),
    Vendors,
    VendorMembers(&'a str),
    VendorRenew(&'a str),
    VendorArchive(&'a str),
    VendorPeriod(&'a str),
    VendorStartConfirm(&'a str),
    VendorBillPreviews(&'a str),
    VendorBillConfirm(&'a str),
    VendorPeriodVoid(&'a str),
    Cycles,
    Cycle(&'a str),
    StartCandidate(&'a str),
    CycleStartConfirm(&'a str),
    CycleBillPreviews(&'a str),
    CycleBillConfirm(&'a str),
    UnmatchedCsv {
        vendor: bool,
        cycle_id: &'a str,
        preview_id: &'a str,
    },
    Previews,
    Expenses,
    Expense(&'a str),
    Settlements,
    SettlementVoid(&'a str),
    NettingBalances,
    NettingPair(&'a str, &'a str),
    NettingPayments,
    NettingPayment(&'a str),
    Orders,
    Order(&'a str),
    OrderActions(&'a str),
}
impl<'a> Route<'a> {
    fn parse(path: &'a str) -> Option<Self> {
        let mut parts = [""; 5];
        let mut len = 0;
        for segment in path.trim_end_matches('/').split('/') {
            if len == parts.len() {
                return None;
            }
            parts[len] = segment;
            len += 1;
        }
        Some(match &parts[..len] {
            ["me"] => Self::Me,
            ["overview"] => Self::Overview,
            ["catalog"] => Self::Catalog,
            ["history"] => Self::History,
            ["inbox"] => Self::Inbox,
            ["inbox", "read"] => Self::InboxRead,
            ["audit"] => Self::Audit,
            ["audit.csv"] => Self::AuditCsv,
            ["operations", "status"] => Self::OperationsStatus,
            ["accounts", "state"] => Self::AccountsState,
            ["accounts", aid, "free"] => Self::AccountFree(aid),
            ["accounts", aid, "upgrade-paid"] => Self::AccountUpgradePaid(aid),
            ["accounts", aid, "deletion-scope"] => Self::DeletionScope(aid),
            ["account-deletions"] => Self::AccountDeletions,
            ["account-deletions", jid, "retry"] => Self::DeletionRetry(jid),
            ["account-deletions", jid, "cancel"] => Self::DeletionCancel(jid),
            ["vendors"] => Self::Vendors,
            ["vendors", vid, "members"] => Self::VendorMembers(vid),
            ["vendors", vid, "renew"] => Self::VendorRenew(vid),
            ["vendors", vid, "archive"] => Self::VendorArchive(vid),
            ["vendor-periods", pid] => Self::VendorPeriod(pid),
            ["vendor-periods", pid, "start-confirm"] => Self::VendorStartConfirm(pid),
            ["vendor-periods", pid, "bill-previews"] => Self::VendorBillPreviews(pid),
            ["vendor-periods", pid, "bill-confirm"] => Self::VendorBillConfirm(pid),
            ["vendor-periods", pid, "void"] => Self::VendorPeriodVoid(pid),
            [
                "vendor-periods",
                pid,
                "bill-previews",
                bpid,
                "unmatched.csv",
            ] => Self::UnmatchedCsv {
                vendor: true,
                cycle_id: pid,
                preview_id: bpid,
            },
            ["cycles"] => Self::Cycles,
            ["cycles", cid] => Self::Cycle(cid),
            ["cycles", cid, "start-candidate"] => Self::StartCandidate(cid),
            ["cycles", cid, "start-confirm"] => Self::CycleStartConfirm(cid),
            ["cycles", cid, "bill-previews"] => Self::CycleBillPreviews(cid),
            ["cycles", cid, "bill-confirm"] => Self::CycleBillConfirm(cid),
            ["cycles", cid, "bill-previews", bpid, "unmatched.csv"] => Self::UnmatchedCsv {
                vendor: false,
                cycle_id: cid,
                preview_id: bpid,
            },
            ["previews"] => Self::Previews,
            ["expenses"] => Self::Expenses,
            ["expenses", eid] => Self::Expense(eid),
            ["settlements"] => Self::Settlements,
            ["settlements", sid, "void"] => Self::SettlementVoid(sid),
            ["netting", "balances"] => Self::NettingBalances,
            ["netting", "balances", a, b] => Self::NettingPair(a, b),
            ["netting", "payments"] => Self::NettingPayments,
            ["netting", "payments", pid] => Self::NettingPayment(pid),
            ["orders"] => Self::Orders,
            ["orders", oid] => Self::Order(oid),
            ["orders", oid, "actions"] => Self::OrderActions(oid),
            _ => return None,
        })
    }
    fn methods(self) -> (&'static [&'static str], &'static str) {
        match self {
            Self::Me
            | Self::Overview
            | Self::Catalog
            | Self::History
            | Self::Inbox
            | Self::Audit
            | Self::AuditCsv
            | Self::OperationsStatus
            | Self::AccountsState
            | Self::DeletionScope(_)
            | Self::AccountDeletions
            | Self::StartCandidate(_)
            | Self::UnmatchedCsv { .. }
            | Self::NettingBalances
            | Self::NettingPair(_, _)
            | Self::NettingPayment(_)
            | Self::Orders
            | Self::Order(_) => (&["GET"], "GET"),
            Self::Cycle(_) => (&["PATCH", "GET"], "PATCH, GET"),
            Self::VendorPeriod(_) => (&["PATCH"], "PATCH"),
            Self::Vendors | Self::Expenses | Self::NettingPayments => {
                (&["GET", "POST"], "GET, POST")
            }
            Self::Expense(_) => (&["GET", "PATCH"], "GET, PATCH"),
            Self::AccountFree(_) => (&["PUT"], "PUT"),
            Self::Cycles
            | Self::Previews
            | Self::Settlements
            | Self::CycleStartConfirm(_)
            | Self::CycleBillPreviews(_)
            | Self::CycleBillConfirm(_)
            | Self::VendorArchive(_)
            | Self::VendorMembers(_)
            | Self::VendorRenew(_)
            | Self::VendorStartConfirm(_)
            | Self::VendorBillPreviews(_)
            | Self::VendorBillConfirm(_)
            | Self::VendorPeriodVoid(_)
            | Self::AccountUpgradePaid(_)
            | Self::SettlementVoid(_)
            | Self::OrderActions(_)
            | Self::InboxRead
            | Self::DeletionRetry(_)
            | Self::DeletionCancel(_) => (&["POST"], "POST"),
        }
    }
}
async fn api(State(app): State<App>, request: Request) -> Result<Response> {
    let _permit = app
        .requests
        .try_acquire()
        .map_err(|_| Error::unavailable())?;
    let (parts, raw_body) = request.into_parts();
    let method = parts.method.as_str();
    let raw_path = parts.uri.path();
    let path = raw_path.strip_prefix("/settlement/api/v1/").unwrap_or("");
    let Some(route) = Route::parse(path) else {
        return Err(Error::new(404, "Not Found"));
    };
    if path.ends_with('/') {
        let target = format!(
            "{}{}{}",
            app.config.origin,
            raw_path.trim_end_matches('/'),
            parts
                .uri
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default()
        );
        return Ok(Redirect::temporary(&target).into_response());
    }
    let (methods, allow) = route.methods();
    if !methods.contains(&method) {
        let mut response = Error::new(405, "Method Not Allowed").into_response();
        response
            .headers_mut()
            .insert("Allow", HeaderValue::from_static(allow));
        return Ok(response);
    }
    let query: BTreeMap<String, String> = parts
        .uri
        .query()
        .and_then(|s| serde_urlencoded::from_str(s).ok())
        .unwrap_or_default();
    let headers = &parts.headers;
    let (actor, sid) = app.authenticate(headers, method != "GET").await?;
    if method != "GET" {
        app.limiter.write(int(&actor, "id")).await?;
    }
    if app.config.read_only && method != "GET" {
        return Err(Error::new(403, "当前实例仅用于只读核验"));
    }
    if let Route::AuditCsv = route {
        let bytes = app.service.audit_csv(&actor, &query).await?;
        return Ok((
            [
                ("Content-Type", "text/csv; charset=utf-8"),
                (
                    "Content-Disposition",
                    "attachment; filename=\"settlement-audit.csv\"",
                ),
            ],
            bytes,
        )
            .into_response());
    }
    if let Route::UnmatchedCsv {
        vendor,
        cycle_id,
        preview_id,
    } = route
    {
        let cycle_id = cycle_id
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .ok_or_else(|| Error::invalid("周期编号无效"))?;
        let bytes = if vendor {
            app.service
                .vendor_unmatched_csv(&actor, cycle_id, preview_id)
                .await?
        } else {
            app.service
                .unmatched_bill_csv(&actor, cycle_id, preview_id)
                .await?
        };
        return Ok((
            [
                ("Content-Type", "text/csv; charset=utf-8"),
                (
                    "Content-Disposition",
                    "attachment; filename=\"settlement-unmatched.csv\"",
                ),
            ],
            bytes,
        )
            .into_response());
    }
    let bytes = axum::body::to_bytes(raw_body, 20 * 1024 * 1024)
        .await
        .map_err(|_| Error::new(413, "请求内容过大"))?;
    let body = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).map_err(|_| Error::invalid("请求 JSON 无效"))?
    };
    let result = app
        .dispatch(method, route, &query, headers, body, (&actor, &sid))
        .await;
    if method != "GET" && result.as_ref().is_err_and(|error| error.status == 409) {
        app.metrics.conflict();
    }
    Ok(Json(result?).into_response())
}
async fn security(request: Request, next: Next) -> Response {
    let immutable_asset = request
        .uri()
        .path()
        .strip_prefix("/settlement/assets/")
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with("__qa__/") && rest != "__qa__");
    let mut response = next.run(request).await;
    let status = response.status();
    let cache_control =
        if immutable_asset && (status.is_success() || status == StatusCode::NOT_MODIFIED) {
            "public, max-age=31536000, immutable"
        } else {
            "no-store"
        };
    for (k, v) in [
        ("Cache-Control", cache_control),
        ("Referrer-Policy", "no-referrer"),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "SAMEORIGIN"),
        (
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'self'; base-uri 'self'; form-action 'self'",
        ),
    ] {
        response
            .headers_mut()
            .insert(k, HeaderValue::from_static(v));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, middleware, routing::get};

    #[test]
    fn missing_entity_is_a_bad_request() {
        assert_eq!(required_entity(None).unwrap_err().status, 400);
        assert_eq!(required_entity(Some(42)).unwrap(), 42);
    }

    #[tokio::test]
    async fn asset_cache_control_is_immutable_only_for_successful_non_qa_assets() {
        let router = Router::new()
            .route(
                "/settlement/assets/index-abc123.js",
                get(|| async { (StatusCode::OK, "ok") }),
            )
            .route(
                "/settlement/assets/vendor-def456.js",
                get(|| async { StatusCode::NOT_MODIFIED }),
            )
            .route(
                "/settlement/assets/__qa__/probe.js",
                get(|| async { (StatusCode::OK, "qa") }),
            )
            .route(
                "/settlement/index.html",
                get(|| async { (StatusCode::OK, "html") }),
            )
            .layer(middleware::from_fn(security));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let client = reqwest::Client::new();
        let cache_for = |path: &'static str| {
            let client = client.clone();
            async move {
                client
                    .get(format!("http://{addr}{path}"))
                    .send()
                    .await
                    .unwrap()
                    .headers()
                    .get("Cache-Control")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            }
        };

        assert_eq!(
            cache_for("/settlement/assets/index-abc123.js").await,
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_for("/settlement/assets/vendor-def456.js").await,
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_for("/settlement/assets/__qa__/probe.js").await,
            "no-store"
        );
        assert_eq!(cache_for("/settlement/assets/missing.js").await, "no-store");
        assert_eq!(cache_for("/settlement/index.html").await, "no-store");
        server.abort();
    }
}
