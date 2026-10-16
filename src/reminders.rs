use crate::{
    error::{Error, Result},
    http::App,
    json::{dumps, int, text},
    service::Service,
    store, time,
};
use chrono::Timelike;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::Mutex;

pub struct ReminderConfig {
    url: reqwest::Url,
    secret: String,
    hour: u32,
}
impl ReminderConfig {
    pub fn new(url: &str, secret: &str, hour: u32, test: bool) -> Result<Self> {
        let url = reqwest::Url::parse(url).map_err(|_| Error::invalid("提醒 Webhook 地址无效"))?;
        let loopback = url.host_str().is_some_and(|h| {
            h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        });
        if !(url.scheme() == "https" || (test && url.scheme() == "http" && loopback))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::invalid(
                "提醒 Webhook 须使用不含登录凭据的 HTTPS 地址",
            ));
        }
        if !(32..=1024).contains(&secret.len()) || hour > 23 {
            return Err(Error::invalid(
                "提醒签名密钥至少 32 字节，发送小时须为 0 至 23",
            ));
        }
        Ok(Self {
            url,
            secret: secret.to_owned(),
            hour,
        })
    }
}
pub struct Reminders {
    config: Option<ReminderConfig>,
    lock: Mutex<()>,
    client: reqwest::Client,
}
impl Reminders {
    pub fn environment(test: bool, read_only: bool) -> Result<Self> {
        let config = if !test
            && !read_only
            && std::env::var("SETTLEMENT_REMINDERS_ENABLED").as_deref() == Ok("true")
        {
            let url = std::env::var("SETTLEMENT_REMINDER_URL").unwrap_or_default();
            let secret = std::env::var("SETTLEMENT_REMINDER_SECRET").unwrap_or_default();
            let hour = std::env::var("SETTLEMENT_REMINDER_HOUR")
                .unwrap_or_else(|_| "9".into())
                .parse()
                .map_err(|_| Error::invalid("提醒发送小时无效"))?;
            Some(ReminderConfig::new(&url, &secret, hour, false)?)
        } else {
            None
        };
        Self::new(config)
    }
    pub fn new(config: Option<ReminderConfig>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::unavailable())?;
        Ok(Self {
            config,
            lock: Mutex::new(()),
            client,
        })
    }
    pub fn enabled(&self) -> bool {
        self.config.is_some()
    }
    pub async fn recover(&self, app: &App) -> Result<()> {
        if self.config.is_none() || app.config.read_only || !app.config.external_writes {
            return Ok(());
        }
        let mut db = app.service.store.write().await?;
        store::exec(&mut db,"UPDATE rust_reminders SET status=CASE WHEN attempts>=5 THEN 'failed' ELSE 'queued' END,next_attempt=?,error='发送结果未确认' WHERE status='sending'",&[json!(time::now())]).await?;
        db.commit().await?;
        Ok(())
    }
    pub async fn tick(&self, app: &App) -> Result<()> {
        let Some(config) = &self.config else {
            return Ok(());
        };
        if app.config.read_only || !app.config.external_writes {
            return Ok(());
        }
        let Ok(_guard) = self.lock.try_lock() else {
            return Ok(());
        };
        let now = time::parse(&time::now())?;
        let local = now.with_timezone(&time::LOCAL_OFFSET);
        if local.hour() >= config.hour {
            let day = local.format("%Y-%m-%d").to_string();
            let cutoff = time::format(now - chrono::Duration::hours(24));
            let mut db = app.service.store.read().await?;
            let pairs=store::all(&mut db,"SELECT min(payer_id,creditor_id) AS a,max(payer_id,creditor_id) AS b FROM orders WHERE status='pending' GROUP BY a,b HAVING min(created_at)<=?",&[json!(cutoff)]).await?;
            drop(db);
            for pair in pairs {
                let (a, b) = (int(&pair, "a"), int(&pair, "b"));
                let mut db = app.service.store.write().await?;
                if store::optional(
                    &mut db,
                    "SELECT 1 FROM rust_reminders WHERE user_a=? AND user_b=? AND reminder_day=?",
                    &[json!(a), json!(b), json!(day)],
                )
                .await?
                .is_some()
                {
                    continue;
                }
                let rows = Service::pending(&mut db, Some((a, b)), None).await?;
                if rows.is_empty() {
                    continue;
                }
                let balance = Service::balance(&mut db, &rows).await?;
                if int(&balance, "amount_fen") == 0 {
                    continue;
                }
                let event = uuid::Uuid::new_v4().to_string();
                let payload = json!({"event_id":event,"event":"pending_payment","created_at":time::format(now),"payer_id":balance["payer_id"],"creditor_id":balance["creditor_id"],"amount_fen":balance["amount_fen"],"currency":"CNY","order_count":balance["order_count"]});
                store::exec(&mut db,"INSERT INTO rust_reminders(id,user_a,user_b,reminder_day,digest,payload,next_attempt,created_at) VALUES(?,?,?,?,?,?,?,?)",&[json!(event),json!(a),json!(b),json!(day),balance["digest"].clone(),json!(dumps(&payload)),json!(time::format(now)),json!(time::format(now))]).await?;
                db.commit().await?;
            }
        }
        let mut db = app.service.store.read().await?;
        let due=store::all(&mut db,"SELECT id FROM rust_reminders WHERE status='queued' AND next_attempt<=? ORDER BY next_attempt,id LIMIT 16",&[json!(time::format(now))]).await?;
        drop(db);
        for item in due {
            self.deliver(app, config, text(&item, "id")).await?;
        }
        Ok(())
    }
    async fn deliver(&self, app: &App, config: &ReminderConfig, id: &str) -> Result<()> {
        let mut db = app.service.store.write().await?;
        let row = store::optional(
            &mut db,
            "SELECT * FROM rust_reminders WHERE id=? AND status='queued'",
            &[json!(id)],
        )
        .await?;
        let Some(row) = row else {
            return Ok(());
        };
        let rows = Service::pending(
            &mut db,
            Some((int(&row, "user_a"), int(&row, "user_b"))),
            None,
        )
        .await?;
        if rows.is_empty() || Service::balance(&mut db, &rows).await?["digest"] != row["digest"] {
            store::exec(
                &mut db,
                "UPDATE rust_reminders SET status='obsolete',error=NULL WHERE id=?",
                &[json!(id)],
            )
            .await?;
            db.commit().await?;
            return Ok(());
        }
        let attempts = int(&row, "attempts") + 1;
        store::exec(
            &mut db,
            "UPDATE rust_reminders SET status='sending',attempts=? WHERE id=?",
            &[json!(attempts), json!(id)],
        )
        .await?;
        db.commit().await?;
        let stamp = (time::epoch() as i64).to_string();
        let body = text(&row, "payload");
        let signature = signature(&config.secret, &stamp, body);
        let response = self
            .client
            .post(config.url.clone())
            .header("Content-Type", "application/json")
            .header("X-Settlement-Event", id)
            .header("X-Settlement-Timestamp", stamp)
            .header("X-Settlement-Signature", format!("v1={signature}"))
            .body(body.to_owned())
            .send()
            .await;
        let success = response.as_ref().is_ok_and(|r| r.status().is_success());
        let status = if success {
            "sent"
        } else if attempts >= 5 {
            "failed"
        } else {
            "queued"
        };
        let delay = 60_i64 * 2_i64.pow((attempts - 1).clamp(0, 6) as u32);
        let mut db = app.service.store.write().await?;
        store::exec(&mut db,"UPDATE rust_reminders SET status=?,sent_at=?,next_attempt=?,error=? WHERE id=? AND status='sending'",&[json!(status),if success{json!(time::now())}else{Value::Null},json!(time::after(delay)),if success{Value::Null}else{json!("提醒发送失败")},json!(id)]).await?;
        db.commit().await?;
        Ok(())
    }
}
pub fn signature(secret: &str, timestamp: &str, body: &str) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
