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
use rand::RngCore;
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
        path: &str,
        query: &BTreeMap<String, String>,
        headers: &HeaderMap,
        body: Value,
        identity: (&Value, &str),
    ) -> Result<Value> {
        let (actor, sid) = identity;
        let s = &self.service;
        let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
        let id = |pos: usize| -> Result<i64> {
            parts
                .get(pos)
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(|| {
                    let name = match parts.as_slice() {
                        ["netting","balances",_,_] => if pos==2 { "user_a" } else { "user_b" },
                        ["netting","payments",_] => "payment_id",
                        _ => match parts[0] { "cycles"=>"cid","expenses"=>"eid","accounts"=>"aid","account-deletions"=>"jid","settlements"=>"sid",_=>"oid" },
                    };
                    Error::validation(json!([{"type":"int_parsing","loc":["path",name],"msg":"Input should be a valid integer, unable to parse string as an integer","input":parts.get(pos).unwrap_or(&"")}]))
                })
        };
        let scope = query.get("scope").map(String::as_str).unwrap_or("mine");
        let query_fields: &[(&str, &[&str])] = match parts.as_slice() {
            ["netting", "payments"] if method == "GET" => &[("scope", &["mine", "all"])],
            ["netting", "balances"] => &[("scope", &["mine", "receivable", "all"])],
            ["orders"] => &[
                ("scope", &["mine", "receivable", "all"]),
                ("view", &["full", "compact"]),
                ("status", &["pending", "submitted", "paid", "void"]),
            ],
            _ => &[],
        };
        crate::input::query_choices(query, query_fields)?;
        if method == "GET" || method == "HEAD" {
            return match parts.as_slice() {
                ["me"] => Ok(actor.clone()),
                ["vendors"] => s.vendors(actor).await,
                ["audit"] => s.audit_log(actor, query, false).await,
                ["operations", "status"] => s.operation_status(actor).await,
                ["overview"] => s.overview(actor).await,
                ["catalog"] => s.catalog(actor).await,
                ["accounts", "state"] => s.account_state(actor).await,
                ["accounts", _, "deletion-scope"] => s.deletion_scope(actor, id(1)?).await,
                ["account-deletions"] => self.deletion_list(actor, None).await,
                ["cycles", _, "start-candidate"] => {
                    s.start_candidate(
                        actor,
                        id(1)?,
                        query.get("after").map(String::as_str).unwrap_or(""),
                    )
                    .await
                }
                ["cycles", _] => s.cycle_detail(actor, id(1)?).await,
                ["expenses"] => s.expenses(actor, None).await,
                ["expenses", _] => s.expenses(actor, Some(id(1)?)).await,
                ["netting", "balances"] => {
                    let mut db = s.store.read().await?;
                    Ok(json!({"balances":Service::balances(&mut db,actor,scope).await?}))
                }
                ["netting", "balances", _, _] => s.netting_balance(actor, id(2)?, id(3)?).await,
                ["netting", "payments"] => s.netting_payments(actor, scope, None).await,
                ["netting", "payments", _] => s.netting_payments(actor, "mine", Some(id(2)?)).await,
                ["orders"] => {
                    let view = query.get("view").map(String::as_str).unwrap_or("full");
                    if !["full", "compact"].contains(&view) {
                        return Err(Error::invalid("订单视图无效"));
                    }
                    let status = query.get("status").map(String::as_str);
                    if status
                        .is_some_and(|s| !["pending", "submitted", "paid", "void"].contains(&s))
                    {
                        return Err(Error::invalid("订单状态无效"));
                    }
                    s.orders(actor, scope, view, status).await
                }
                ["orders", _] => s.detail(actor, id(1)?).await,
                ["history"] => s.history(actor).await,
                ["inbox"] => s.inbox(actor).await,
                _ => Err(Error::new(404, "Not Found")),
            };
        }
        let (kind, entity) = match (method, parts.as_slice()) {
            ("POST", ["vendors"]) => ("create-vendor", None),
            ("PATCH", ["vendors", _]) => ("edit-vendor", Some(id(1)?)),
            ("POST", ["vendors", _, "members"]) => ("add-vendor-members", Some(id(1)?)),
            ("POST", ["vendors", _, "renew"]) => ("renew-vendor", Some(id(1)?)),
            ("POST", ["vendors", _, "archive"]) => ("archive-vendor", Some(id(1)?)),
            ("POST", ["vendors", _, "periods"]) => ("create-vendor-period", Some(id(1)?)),
            ("PATCH", ["vendor-periods", _]) => ("edit-vendor-period", Some(id(1)?)),
            ("POST", ["vendor-periods", _, "start-confirm"]) => {
                ("vendor-start-confirm", Some(id(1)?))
            }
            ("POST", ["vendor-periods", _, "bill-previews"]) => {
                let payload = crate::input::normalize("bill-preview", body)?;
                return s.vendor_bill_preview(actor, id(1)?, &payload).await;
            }
            ("POST", ["vendor-periods", _, "bill-confirm"]) => {
                ("vendor-bill-confirm", Some(id(1)?))
            }
            ("POST", ["vendor-periods", _, "void"]) => ("void-vendor", Some(id(1)?)),
            ("POST", ["cycles"]) => ("create", None),
            ("PATCH", ["cycles", _]) => ("edit", Some(id(1)?)),
            ("POST", ["cycles", _, "start-confirm"]) => ("start-confirm", Some(id(1)?)),
            ("PUT", ["accounts", _, "free"]) => ("free", Some(id(1)?)),
            ("POST", ["accounts", _, "upgrade-paid"]) => ("upgrade-paid", Some(id(1)?)),
            ("POST", ["cycles", _, "bill-previews"]) => {
                let payload = crate::input::normalize("bill-preview", body)?;
                return s.bill_preview(actor, id(1)?, &payload).await;
            }
            ("POST", ["cycles", _, "bill-confirm"]) => ("bill-confirm", Some(id(1)?)),
            ("POST", ["previews"]) => {
                let payload = crate::input::normalize("preview", body)?;
                return s.preview(actor, &payload).await;
            }
            ("POST", ["expenses"]) => ("create-expense", None),
            ("PATCH", ["expenses", _]) => ("edit-expense", Some(id(1)?)),
            ("POST", ["settlements"]) => ("settle", None),
            ("POST", ["settlements", _, "void"]) => ("void", Some(id(1)?)),
            ("POST", ["netting", "payments"]) => ("netting-payment", None),
            ("POST", ["orders", _, "actions"]) => ("transition", Some(id(1)?)),
            ("POST", ["batches"]) => ("merge", None),
            ("POST", ["inbox", "read"]) => ("inbox_read", None),
            ("POST", ["account-deletions", _, "retry"]) => ("delete-retry", Some(id(1)?)),
            ("POST", ["account-deletions", _, "cancel"]) => ("delete-cancel", Some(id(1)?)),
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
                    admin(actor)?;
                    prepared.bill = Some(
                        s.prepare_bill(actor, required_entity(entity)?, &payload)
                            .await?,
                    );
                }
                "start-confirm" => {
                    admin(actor)?;
                    s.sync_selection(
                        &[json!(required_entity(entity)?)],
                        &time::now(),
                        payload["start"]
                            .as_str()
                            .map(crate::input::timestamp)
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
            self.deletion_wake.notify_one();
            let items = self
                .deletion_list(actor, Some(&[required_entity(entity)?]))
                .await?;
            return Ok(items["items"][0].clone());
        }
        Ok(result)
    }
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
    rand::thread_rng().fill_bytes(&mut bytes);
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
fn route_methods(path: &str) -> Option<&'static [&'static str]> {
    match path
        .trim_end_matches('/')
        .split('/')
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["me" | "overview" | "catalog" | "history" | "inbox" | "account-deletions"]
        | ["audit" | "audit.csv"]
        | ["operations", "status"]
        | ["accounts", "state"]
        | ["accounts", _, "deletion-scope"]
        | ["cycles", _, "start-candidate"]
        | ["cycles", _, "bill-previews", _, "unmatched.csv"]
        | ["vendor-periods", _, "bill-previews", _, "unmatched.csv"]
        | ["netting", "balances"]
        | ["netting", "balances", _, _]
        | ["netting", "payments", _]
        | ["orders"]
        | ["orders", _] => Some(&["GET"]),
        ["cycles", _] => Some(&["PATCH", "GET"]),
        ["vendors" | "vendor-periods", _] => Some(&["PATCH"]),
        ["vendors"] | ["expenses"] | ["netting", "payments"] => Some(&["GET", "POST"]),
        ["expenses", _] => Some(&["GET", "PATCH"]),
        ["accounts", _, "free"] => Some(&["PUT"]),
        ["cycles" | "previews" | "settlements" | "batches"]
        | [
            "cycles",
            _,
            "start-confirm" | "bill-previews" | "bill-confirm",
        ]
        | ["vendors", _, "periods" | "archive" | "members" | "renew"]
        | [
            "vendor-periods",
            _,
            "start-confirm" | "bill-previews" | "bill-confirm" | "void",
        ]
        | ["accounts", _, "upgrade-paid"]
        | ["settlements", _, "void"]
        | ["orders", _, "actions"]
        | ["inbox", "read"]
        | ["account-deletions", _, "retry" | "cancel"] => Some(&["POST"]),
        _ => None,
    }
}
async fn api(State(app): State<App>, request: Request) -> Result<Response> {
    let _permit = app
        .requests
        .try_acquire()
        .map_err(|_| Error::unavailable())?;
    let method = request.method().as_str().to_owned();
    let path = request
        .uri()
        .path()
        .strip_prefix("/settlement/api/v1/")
        .unwrap_or("")
        .to_owned();
    let Some(methods) = route_methods(&path) else {
        return Err(Error::new(404, "Not Found"));
    };
    if path.ends_with('/') {
        let target = format!(
            "{}{}{}",
            app.config.origin,
            request.uri().path().trim_end_matches('/'),
            request
                .uri()
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default()
        );
        return Ok(Redirect::temporary(&target).into_response());
    }
    if !methods.contains(&method.as_str()) {
        let mut response = Error::new(405, "Method Not Allowed").into_response();
        response
            .headers_mut()
            .insert("Allow", HeaderValue::from_static(methods[0]));
        return Ok(response);
    }
    let query = params(&request);
    let headers = request.headers().clone();
    let (actor, sid) = app
        .authenticate(&headers, method != "GET" && method != "HEAD")
        .await?;
    if method != "GET" && method != "HEAD" {
        app.limiter.write(int(&actor, "id")).await?;
    }
    if app.config.read_only && method != "GET" && method != "HEAD" {
        return Err(Error::new(403, "当前实例仅用于只读核验"));
    }
    if method == "GET" && path == "audit.csv" {
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
    if method == "GET"
        && let [
            source @ ("cycles" | "vendor-periods"),
            cycle_id,
            "bill-previews",
            preview_id,
            "unmatched.csv",
        ] = path.split('/').collect::<Vec<_>>().as_slice()
    {
        let cycle_id = cycle_id
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .ok_or_else(|| Error::invalid("周期编号无效"))?;
        let bytes = if *source == "vendor-periods" {
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
    let bytes = axum::body::to_bytes(request.into_body(), 20 * 1024 * 1024)
        .await
        .map_err(|_| Error::new(413, "请求内容过大"))?;
    let body = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).map_err(|_| Error::invalid("请求 JSON 无效"))?
    };
    let result = app
        .dispatch(&method, &path, &query, &headers, body, (&actor, &sid))
        .await;
    if method != "GET" && result.as_ref().is_err_and(|error| error.status == 409) {
        app.metrics.conflict();
    }
    Ok(Json(result?).into_response())
}
async fn security(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    for (k, v) in [
        ("Cache-Control", "no-store"),
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

    #[test]
    fn missing_entity_is_a_bad_request() {
        assert_eq!(required_entity(None).unwrap_err().status, 400);
        assert_eq!(required_entity(Some(42)).unwrap(), 42);
    }
}
