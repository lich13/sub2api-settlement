use crate::{
    error::{Error, Result},
    http::App,
    json::{flag, int, text, without},
    service::{Db, admin, audit, one},
    store::{self, all, exec},
    time,
};
use serde_json::{Value, json};
pub fn public_job(row: Value) -> Value {
    without(row, &["session_id", "account_created_at"])
}
pub async fn change(
    db: &mut Db,
    actor: &Value,
    jid: i64,
    version: i64,
    action: &str,
    session: Option<&str>,
) -> Result<Value> {
    admin(actor)?;
    let row = one(
        db,
        "SELECT * FROM account_deletions WHERE id=?",
        &[json!(jid)],
    )
    .await?;
    if int(&row, "version") != version {
        return Err(Error::conflict("删除任务已更新，请刷新"));
    }
    if row["status"] != "failed" && row["status"] != "queued" {
        return Err(Error::conflict("删除任务正在核验或已结束，不能修改"));
    }
    if action == "retry" && row["status"] != "failed" {
        return Err(Error::conflict("删除任务已经排队，请勿重复提交"));
    }
    let target = if action == "retry" {
        "queued"
    } else {
        "cancelled"
    };
    exec(db,"UPDATE account_deletions SET status=?,actor_id=?,session_id=?,error=NULL,version=version+1,updated_at=? WHERE id=?",&[json!(target),actor["id"].clone(),json!(if action=="retry"{session}else{None}),json!(time::now()),json!(jid)]).await?;
    audit(
        db,
        actor,
        if action == "retry" {
            "retry_account_deletion"
        } else {
            "cancel_account_deletion"
        },
        row["account_id"].clone(),
        &json!({"deletion_id":jid}),
    )
    .await?;
    Ok(json!({"id":jid,"status":target}))
}
impl App {
    pub async fn deletion_list(&self, actor: &Value, ids: Option<&[i64]>) -> Result<Value> {
        admin(actor)?;
        let mut db = self.service.store.read().await?;
        Ok(
            json!({"items":all(&mut db,"SELECT * FROM account_deletions ORDER BY id DESC",&[]).await?.into_iter().filter(|r|ids.is_none_or(|ids|ids.contains(&int(r,"id")))).map(public_job).collect::<Vec<_>>()}),
        )
    }
    pub async fn recover_deletions(&self) -> Result<()> {
        let mut db = self.service.store.write().await?;
        exec(&mut db,"UPDATE account_deletions SET status='verify',version=version+1,updated_at=? WHERE status='running'",&[json!(time::now())]).await?;
        db.commit().await?;
        Ok(())
    }
    pub async fn pending_deletions(&self) -> Result<()> {
        if !self.config.external_writes {
            return Ok(());
        }
        let mut db = self.service.store.read().await?;
        let ids = all(
            &mut db,
            "SELECT id FROM account_deletions WHERE status IN ('queued','verify') ORDER BY id LIMIT 16",
            &[],
        )
        .await?;
        drop(db);
        for id in ids {
            self.process_deletion(int(&id, "id")).await?;
        }
        Ok(())
    }
    async fn finish_deletion(&self, job: &Value, status: &str, error: Option<&str>) -> Result<()> {
        let mut db = self.service.store.write().await?;
        self.service.inventory.persist(&mut db).await?;
        let changed=exec(&mut db,"UPDATE account_deletions SET status=?,error=?,version=version+1,updated_at=? WHERE id=? AND status='running'",&[json!(status),json!(error),json!(time::now()),job["id"].clone()]).await?.rows_affected();
        if changed != 0 {
            audit(&mut db,&json!({"id":job["actor_id"]}),&format!("account_deletion_{status}"),job["account_id"].clone(),&json!({"deletion_id":job["id"],"settlement_id":job["settlement_id"],"error":error})).await?;
        }
        db.commit().await?;
        Ok(())
    }
    async fn probe_deletion(&self, job: &Value) -> &'static str {
        let state = self.service.inventory.refresh(true).await;
        if !state["error"].is_null() {
            return "unknown";
        }
        let snapshot = self.service.inventory.state.read().await;
        let aid = int(job, "account_id");
        let root = snapshot.rows.get(&aid);
        let has_children = snapshot
            .rows
            .values()
            .any(|a| int(a, "root_id") == aid && !flag(a, "deleted"));
        if root.is_none_or(|a| flag(a, "deleted")) && !has_children {
            return "deleted";
        }
        if root.is_some_and(|a| a["created_at"] != job["account_created_at"]) {
            return "changed";
        }
        "present"
    }
    pub async fn process_deletion(&self, jid: i64) -> Result<()> {
        if !self.config.external_writes {
            return Err(Error::new(403, "当前实例禁止外部写操作"));
        }
        let _guard = self.deletion_lock.lock().await;
        let mut db = self.service.store.write().await?;
        let job = one(
            &mut db,
            "SELECT * FROM account_deletions WHERE id=?",
            &[json!(jid)],
        )
        .await?;
        if job["status"] != "queued" && job["status"] != "verify" {
            return Ok(());
        }
        exec(&mut db,"UPDATE account_deletions SET status='running',version=version+1,updated_at=? WHERE id=?",&[json!(time::now()),json!(jid)]).await?;
        db.commit().await?;
        let result = async {
            match self.probe_deletion(&job).await {
                "deleted" => return self.finish_deletion(&job, "done", None).await,
                "unknown" => {
                    return self
                        .finish_deletion(&job, "verify", Some("删除状态暂不可核验"))
                        .await;
                }
                "changed" => {
                    return self
                        .finish_deletion(&job, "failed", Some("账号身份已变化，未执行删除"))
                        .await;
                }
                _ => {}
            }
            if job["status"] == "verify" {
                return self
                    .finish_deletion(&job, "failed", Some("账号仍存在，请重试删除"))
                    .await;
            }
            let mut db = self.service.store.read().await?;
            let s = one(
                &mut db,
                "SELECT status FROM settlements WHERE id=?",
                &[job["settlement_id"].clone()],
            )
            .await?;
            let open = store::optional(
                &mut db,
                "SELECT 1 AS found FROM cycles WHERE account_id=? AND status='open'",
                &[job["account_id"].clone()],
            )
            .await?
            .is_some();
            drop(db);
            if s["status"] != "active" || open {
                return self
                    .finish_deletion(&job, "failed", Some("结算或周期已变化，未执行删除"))
                    .await;
            }
            let error = self.delete_upstream(&job).await;
            match self.probe_deletion(&job).await {
                "deleted" => self.finish_deletion(&job, "done", None).await,
                "unknown" => {
                    self.finish_deletion(&job, "verify", Some("删除结果待核验，请勿重复结算"))
                        .await
                }
                _ => {
                    self.finish_deletion(
                        &job,
                        "failed",
                        Some(error.unwrap_or("账号删除未完成，可单独重试")),
                    )
                    .await
                }
            }
        }
        .await;
        if result.is_err() {
            self.finish_deletion(&job, "verify", Some("删除结果待核验，请勿重复结算"))
                .await?;
        }
        Ok(())
    }
    async fn delete_upstream(&self, job: &Value) -> Option<&'static str> {
        let session = async {
            let mut db = self.service.store.read().await?;
            store::optional(
                &mut db,
                "SELECT * FROM sessions WHERE id=?",
                &[job["session_id"].clone()],
            )
            .await
        }
        .await
        .ok()
        .flatten();
        let Some(session) = session else {
            return Some("登录已失效，请重新进入后重试删除");
        };
        if session["expires_at"].as_f64().unwrap_or(0.0) <= time::epoch() {
            return Some("登录已失效，请重新进入后重试删除");
        }
        let result = async {
            let token = self.decrypt(text(&session, "token"))?;
            let actor = self.validate(&token).await?;
            admin(&actor)?;
            Ok::<_, Error>((token, actor))
        }
        .await;
        let Ok((token, actor)) = result else {
            return Some("管理员校验未通过，请重新登录后重试删除");
        };
        if actor["id"] != job["actor_id"] || actor["id"] != session["user_id"] {
            return Some("管理员身份已变化，请重新登录后重试删除");
        }
        let response = self
            .client
            .delete(format!(
                "{}/api/v1/admin/accounts/{}",
                self.config.verify_url,
                int(job, "account_id")
            ))
            .bearer_auth(token)
            .send()
            .await;
        let Ok(response) = response else {
            return Some("删除请求未得到确认，请核对账号状态后重试");
        };
        if [401, 403].contains(&response.status().as_u16()) {
            return Some("管理员权限已失效，请重新登录后重试删除");
        }
        if !response.status().is_success() {
            return Some("Sub2API 未确认删除，请核对账号状态后重试");
        }
        match response.json::<Value>().await {
            Ok(v) if v["code"] == 0 => None,
            Ok(_) => Some("Sub2API 未确认删除，请核对账号状态后重试"),
            Err(_) => Some("删除请求未得到确认，请核对账号状态后重试"),
        }
    }
}
