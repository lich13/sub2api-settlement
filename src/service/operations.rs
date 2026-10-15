use super::*;
use crate::store::{all, optional};

fn positive_query(query: &BTreeMap<String, String>, key: &str, default: i64) -> Result<i64> {
    query.get(key).map_or(Ok(default), |s| {
        s.parse::<i64>()
            .ok()
            .filter(|v| *v > 0)
            .ok_or_else(|| Error::invalid("分页参数无效"))
    })
}

/// Neutralize formulas before CSV quoting, including whitespace-prefixed formulas.
pub fn csv_cell(value: &str) -> String {
    let first =
        value.trim_start_matches(|c: char| c.is_whitespace() || c.is_control() || c == '\u{feff}');
    if first.starts_with(['=', '+', '-', '@']) {
        format!("'{value}")
    } else {
        value.to_owned()
    }
}

pub fn csv_bytes(headers: &[&str], rows: impl IntoIterator<Item = Vec<String>>) -> Result<Vec<u8>> {
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(Vec::new());
    writer
        .write_record(headers)
        .map_err(|_| Error::unavailable())?;
    for row in rows {
        writer
            .write_record(row.iter().map(|v| csv_cell(v)))
            .map_err(|_| Error::unavailable())?;
    }
    let bytes = writer.into_inner().map_err(|_| Error::unavailable())?;
    Ok([b"\xef\xbb\xbf".as_slice(), &bytes].concat())
}

impl Service {
    pub async fn unmatched_bill_csv(
        &self,
        actor: &Value,
        cycle_id: i64,
        preview_id: &str,
    ) -> Result<Vec<u8>> {
        admin(actor)?;
        let mut db = self.store.read().await?;
        let preview = store::one(
            &mut db,
            "SELECT * FROM bill_previews WHERE id=? AND cycle_id=? AND actor_id=?",
            &[json!(preview_id), json!(cycle_id), actor["id"].clone()],
            "账单预览不存在，请重新匹配",
        )
        .await?;
        if text(&preview, "expires_at") <= time::now().as_str() {
            return Err(Error::conflict("账单预览已过期，请重新匹配"));
        }
        let cycle = store::one(
            &mut db,
            "SELECT version,status FROM cycles WHERE id=?",
            &[json!(cycle_id)],
            "周期不存在",
        )
        .await?;
        let payload: Value = serde_json::from_str(text(&preview, "payload"))?;
        let result = &payload["result"];
        if cycle["status"] != "open" || cycle["version"] != result["version"] {
            return Err(Error::conflict("周期已修改或已出单，请重新匹配"));
        }
        csv_bytes(
            &[
                "CSV 行号",
                "请求时间（UTC）",
                "模型",
                "请求标识",
                "输入 Token",
                "输出 Token",
                "缓存读取 Token",
                "缓存创建 Token",
                "上游扣费（元，原始精度）",
                "未匹配原因",
            ],
            crate::json::array(&result["unmatched"])
                .iter()
                .map(|row| (row, false))
                .chain(
                    crate::json::array(&result["outside"])
                        .iter()
                        .map(|row| (row, true)),
                )
                .map(|(row, outside)| {
                    let reason = if outside {
                        "周期范围外"
                    } else if row["reason"] == "ambiguous" {
                        "存在歧义"
                    } else {
                        "未找到本地请求"
                    };
                    vec![
                        int(row, "row").to_string(),
                        text(row, "created_at").to_owned(),
                        text(row, "model").to_owned(),
                        text(row, "request_id").to_owned(),
                        int(row, "input_tokens").to_string(),
                        int(row, "output_tokens").to_string(),
                        int(row, "cache_read_tokens").to_string(),
                        int(row, "cache_creation_tokens").to_string(),
                        text(row, "amount").to_owned(),
                        reason.to_owned(),
                    ]
                }),
        )
    }
    pub async fn deletion_scope(&self, actor: &Value, account_id: i64) -> Result<Value> {
        admin(actor)?;
        if !self.inventory.refresh(true).await["error"].is_null() {
            return Err(Error::new(503, "无法核验删除范围，请稍后重试"));
        }
        let snapshot = self.inventory.state.read().await;
        crate::deletions::scope(account_id, snapshot.rows.values().cloned().collect())
    }
    pub async fn audit_log(
        &self,
        actor: &Value,
        query: &BTreeMap<String, String>,
        export: bool,
    ) -> Result<Value> {
        admin(actor)?;
        let before = positive_query(query, "before", i64::MAX)?;
        let limit = if export {
            100_000
        } else {
            positive_query(query, "limit", 50)?.min(200)
        };
        let action = query.get("action").map(String::as_str).unwrap_or("");
        if action.len() > 64
            || !action
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(Error::invalid("操作类型无效"));
        }
        let from = query.get("from").map(|v| time::instant(v)).transpose()?;
        let to = query.get("to").map(|v| time::instant(v)).transpose()?;
        if from.as_ref().zip(to.as_ref()).is_some_and(|(a, b)| a >= b) {
            return Err(Error::invalid("结束时间必须晚于开始时间"));
        }
        let mut db = self.store.read().await?;
        let mut items = all(&mut db,
            "SELECT a.*,COALESCE(u.name,'用户 #'||a.actor_id) AS actor_name FROM audit a LEFT JOIN users u ON u.id=a.actor_id WHERE a.id<? AND (?='' OR a.action=?) AND (? IS NULL OR a.created_at>=?) AND (? IS NULL OR a.created_at<?) ORDER BY a.id DESC LIMIT ?",
            &[json!(before),json!(action),json!(action),json!(from),json!(from),json!(to),json!(to),json!(limit+1)]).await?;
        let more = items.len() > limit as usize;
        if export && more {
            return Err(Error::new(413, "记录超过十万条，请缩小导出时间范围"));
        }
        items.truncate(limit as usize);
        for item in &mut items {
            item["payload"] = serde_json::from_str(text(item, "payload"))?;
        }
        let next = if more {
            items.last().map(|v| v["id"].clone()).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        Ok(json!({"items":items,"next_before":next}))
    }
    pub async fn audit_csv(
        &self,
        actor: &Value,
        query: &BTreeMap<String, String>,
    ) -> Result<Vec<u8>> {
        let result = self.audit_log(actor, query, true).await?;
        csv_bytes(
            &[
                "记录编号",
                "时间（UTC）",
                "操作人",
                "操作",
                "关联编号",
                "内容",
            ],
            crate::json::array(&result["items"]).iter().map(|v| {
                vec![
                    int(v, "id").to_string(),
                    text(v, "created_at").to_owned(),
                    text(v, "actor_name").to_owned(),
                    text(v, "action").to_owned(),
                    v["entity_id"].to_string(),
                    dumps(&v["payload"]),
                ]
            }),
        )
    }
    pub async fn operation_status(&self, actor: &Value) -> Result<Value> {
        admin(actor)?;
        let accounts = self.inventory.state.read().await.state();
        let mut db = self.store.read().await?;
        let checkpoint = optional(&mut db, "SELECT * FROM rust_sync_state WHERE id=1", &[]).await?;
        let jobs = store::one(&mut db,"SELECT COUNT(*) AS count FROM account_deletions WHERE status NOT IN ('done','cancelled')",&[],"删除任务状态不可用").await?;
        Ok(
            json!({"last_sync":store::meta(&mut db,"last_sync").await?,"sync_error":store::meta(&mut db,"sync_error").await?,"accounts":accounts,"checkpoint":checkpoint,"pending_deletions":jobs["count"]}),
        )
    }
}
