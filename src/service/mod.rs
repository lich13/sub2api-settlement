mod calculation;
mod cycles;
mod ledger;
pub mod operations;
mod presentation;
mod reads;
mod sync;
pub mod vendor_billing;
mod vendor_groups;
mod vendors;
use crate::{
    error::{Error, Result},
    inventory::Inventory,
    json::{digest, dumps, int, text},
    source::Source,
    store::{self, Store},
    time,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Instant};
use tokio::sync::{Mutex, Semaphore};
pub type Db = sqlx::SqliteConnection;
#[derive(Clone)]
pub struct Service {
    pub store: Store,
    pub source: Source,
    pub inventory: Inventory,
    pub sync_lock: Arc<Mutex<Option<Instant>>>,
    pub cpu: Arc<Semaphore>,
}
#[derive(Default)]
pub struct Prepared {
    pub calculations: BTreeMap<i64, (Value, i64)>,
    pub expenses: BTreeMap<i64, Value>,
    pub bill: Option<(Value, i64)>,
    pub vendor_bill: Option<Value>,
}
impl Service {
    pub async fn new(store: Store, source: Source) -> Result<Self> {
        let inventory = Inventory::new(store.clone(), source.clone()).await?;
        Ok(Self {
            store,
            source,
            inventory,
            sync_lock: Arc::new(Mutex::new(None)),
            cpu: Arc::new(Semaphore::new(
                std::thread::available_parallelism()
                    .map(usize::from)
                    .unwrap_or(2)
                    .clamp(1, 4),
            )),
        })
    }
    pub async fn replay(
        &self,
        actor: &Value,
        key: &str,
        action: &str,
        payload: &Value,
    ) -> Result<Option<Value>> {
        valid_key(key)?;
        let mut db = self.store.read().await?;
        if let Some(p) = store::optional(
            &mut db,
            "SELECT * FROM requests WHERE actor_id=? AND key=?",
            &[actor["id"].clone(), json!(key)],
        )
        .await?
        {
            if p["signature"] != digest(&json!([action, payload])) {
                return Err(Error::conflict("操作编号已用于其他请求"));
            }
            return Ok(Some(serde_json::from_str(text(&p, "response"))?));
        }
        Ok(None)
    }
    pub async fn mutate(
        &self,
        actor: &Value,
        key: &str,
        action: &str,
        payload: &Value,
        prepared: Prepared,
        session_id: Option<&str>,
    ) -> Result<Value> {
        valid_key(key)?;
        let signature = digest(&json!([action, payload]));
        let mut db = self.store.write().await?;
        if let Some(p) = store::optional(
            &mut db,
            "SELECT * FROM requests WHERE actor_id=? AND key=?",
            &[actor["id"].clone(), json!(key)],
        )
        .await?
        {
            if p["signature"] != signature {
                return Err(Error::conflict("操作编号已用于其他请求"));
            }
            return Ok(serde_json::from_str(text(&p, "response"))?);
        }
        self.inventory.persist(&mut db).await?;
        let (kind, id) = action
            .split_once(':')
            .map(|(a, b)| (a, b.parse::<i64>().unwrap_or(0)))
            .unwrap_or((action, 0));
        let result = match kind {
            "add-vendor-members" => self.add_vendor_members(&mut db, actor, id, payload).await?,
            "renew-vendor" => self.renew_vendor(&mut db, actor, id, payload).await?,
            "create-vendor" => self.save_vendor(&mut db, actor, None, payload).await?,
            "edit-vendor" => self.save_vendor(&mut db, actor, Some(id), payload).await?,
            "void-vendor" => self.void_vendor(&mut db, actor, id, payload).await?,
            "archive-vendor" => self.archive_vendor(&mut db, actor, id, payload).await?,
            "create-vendor-period" => {
                self.save_vendor_period(&mut db, actor, id, payload, false)
                    .await?
            }
            "edit-vendor-period" => {
                self.save_vendor_period(&mut db, actor, id, payload, true)
                    .await?
            }
            "vendor-start-confirm" => {
                self.confirm_vendor_start(&mut db, actor, id, payload)
                    .await?
            }
            "vendor-bill-confirm" => {
                self.confirm_vendor_bill(&mut db, actor, id, payload, prepared.vendor_bill)
                    .await?
            }
            "create" => self.create_cycle(&mut db, actor, payload).await?,
            "edit" => self.edit_cycle(&mut db, actor, id, payload).await?,
            "start-confirm" => self.confirm_start(&mut db, actor, id, payload).await?,
            "free" => self.free(&mut db, actor, id, payload).await?,
            "upgrade-paid" => self.upgrade_paid(&mut db, actor, id, payload).await?,
            "create-expense" => self.save_expense(&mut db, actor, payload, None).await?,
            "edit-expense" => self.save_expense(&mut db, actor, payload, Some(id)).await?,
            "bill-confirm" => {
                self.confirm_bill(&mut db, actor, id, payload, prepared.bill)
                    .await?
            }
            "settle" => {
                self.settle(&mut db, actor, payload, prepared, session_id)
                    .await?
            }
            "void" => self.void(&mut db, actor, id).await?,
            "netting-payment" => self.pay_netting(&mut db, actor, payload).await?,
            "merge" => self.merge(&mut db, actor, payload).await?,
            "transition" => {
                self.transition(
                    &mut db,
                    actor,
                    id,
                    text(payload, "action"),
                    int(payload, "version"),
                )
                .await?
            }
            "delete-retry" | "delete-cancel" => {
                crate::deletions::change(
                    &mut db,
                    actor,
                    id,
                    int(payload, "version"),
                    if kind == "delete-retry" {
                        "retry"
                    } else {
                        "cancel"
                    },
                    session_id,
                    payload.get("scope_digest").and_then(Value::as_str),
                )
                .await?
            }
            "inbox_read" => {
                store::exec(
                    &mut db,
                    "UPDATE notifications SET read=1 WHERE user_id=?",
                    &[actor["id"].clone()],
                )
                .await?;
                json!({"ok":true})
            }
            _ => return Err(Error::new(404, "记录不存在")),
        };
        store::exec(
            &mut db,
            "INSERT INTO requests VALUES(?,?,?,?)",
            &[
                actor["id"].clone(),
                json!(key),
                json!(signature),
                json!(dumps(&result)),
            ],
        )
        .await?;
        db.commit().await?;
        Ok(result)
    }
    pub async fn fresh(db: &mut Db) -> Result<()> {
        let last = store::meta(db, "last_sync").await?;
        let error = store::meta(db, "sync_error").await?;
        if !error.is_null()
            || last.is_null()
            || time::parse(&time::now())? - time::parse(last.as_str().unwrap_or(""))?
                > chrono::Duration::minutes(3)
        {
            return Err(Error::new(503, "消费记录尚未同步，暂不能结算"));
        }
        Ok(())
    }
    pub async fn assert_not_deleting(db: &mut Db, account: i64) -> Result<()> {
        if store::optional(db,"SELECT 1 AS found FROM account_deletions WHERE account_id=? AND status IN ('queued','running','verify')",&[json!(account)]).await?.is_some(){return Err(Error::conflict("账号删除处理中，请等待核验完成"));}
        Ok(())
    }
}
pub fn admin(actor: &Value) -> Result<()> {
    if text(actor, "role") != "admin" {
        return Err(Error::new(403, "仅管理员可执行此操作"));
    }
    Ok(())
}
pub fn valid_key(key: &str) -> Result<()> {
    if key.is_empty() || key.chars().count() > 128 {
        return Err(Error::new(400, "请求缺少有效的操作编号"));
    }
    Ok(())
}
pub async fn audit(
    db: &mut Db,
    actor: &Value,
    action: &str,
    entity: Value,
    payload: &Value,
) -> Result<()> {
    store::exec(
        db,
        "INSERT INTO audit(actor_id,action,entity_id,payload,created_at) VALUES(?,?,?,?,?)",
        &[
            actor["id"].clone(),
            json!(action),
            entity,
            json!(dumps(payload)),
            json!(time::now()),
        ],
    )
    .await?;
    Ok(())
}
pub async fn notify(db: &mut Db, uid: i64, title: &str, order: Value) -> Result<()> {
    store::exec(
        db,
        "INSERT INTO notifications(user_id,title,order_id,created_at) VALUES(?,?,?,?)",
        &[json!(uid), json!(title), order, json!(time::now())],
    )
    .await?;
    Ok(())
}
pub async fn name(db: &mut Db, uid: i64) -> Result<String> {
    Ok(
        store::optional(db, "SELECT name FROM users WHERE id=?", &[json!(uid)])
            .await?
            .map(|v| text(&v, "name").to_owned())
            .unwrap_or_else(|| format!("用户 #{uid}")),
    )
}
pub async fn one(db: &mut Db, sql: &str, args: &[Value]) -> Result<Value> {
    store::one(db, sql, args, "记录不存在").await
}
