use super::*;
use crate::{
    json::{array, flag, merge, without},
    service::{
        cycles::{estimate, start_meta},
        ledger::authorize,
    },
    store::{all, optional},
};
impl Service {
    pub async fn account_state(&self, actor: &Value) -> Result<Value> {
        let mut state = self.inventory.refresh(false).await;
        if text(actor, "role") != "admin" {
            let mut db = self.store.read().await?;
            let configured = all(
                &mut db,
                "SELECT account_id FROM profiles WHERE kind='paid'",
                &[],
            )
            .await?
            .iter()
            .map(|r| int(r, "account_id"))
            .collect::<std::collections::BTreeSet<_>>();
            state["active_ids"] = json!(
                array(&state["active_ids"])
                    .iter()
                    .filter(|id| id.as_i64().is_some_and(|id| configured.contains(&id)))
                    .cloned()
                    .collect::<Vec<_>>()
            );
            state["accounts"] = json!(
                array(&state["accounts"])
                    .iter()
                    .filter(|a| configured.contains(&int(a, "id")))
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
        Ok(state)
    }
    pub async fn catalog(&self, actor: &Value) -> Result<Value> {
        admin(actor)?;
        let (state, mut accounts) = self.inventory.active_snapshot().await;
        let mut db = self.store.read().await?;
        for a in &mut accounts {
            let earliest = store::optional(
                &mut db,
                "SELECT id,created_at FROM usage WHERE root_id=? ORDER BY created_at,id LIMIT 1",
                &[a["id"].clone()],
            )
            .await?
            .unwrap_or(Value::Null);
            a["earliest_usage_at"] = earliest["created_at"].clone();
            a["earliest_usage_id"] = earliest["id"].clone();
        }
        Ok(
            json!({"accounts":accounts,"account_revision":state["revision"],"account_sync_error":state["error"],"users":all(&mut db,"SELECT id,name FROM users WHERE deleted=0 AND status='active' ORDER BY id",&[]).await?,"coverage_from":store::meta(&mut db,"coverage_from").await?}),
        )
    }
    pub async fn overview(&self, actor: &Value) -> Result<Value> {
        let (state, accounts) = self.inventory.active_snapshot().await;
        let mut db = self.store.read().await?;
        let mut rows = Vec::new();
        let mut vendor_cache = BTreeMap::<i64, Value>::new();
        let mut sync_error = store::meta(&mut db, "sync_error").await?;
        let last = store::meta(&mut db, "last_sync").await?;
        if (last.is_null()
            || time::parse(&time::now())? - time::parse(last.as_str().unwrap_or(""))?
                > chrono::Duration::minutes(3))
            && sync_error.is_null()
        {
            sync_error = json!("消费记录同步已中断，请稍后重试");
        }
        for a in accounts {
            let p=store::optional(&mut db,"SELECT name AS public_name,kind,version AS profile_version FROM profiles WHERE account_id=?",&[a["id"].clone()]).await?.unwrap_or(json!({"public_name":null,"kind":null,"profile_version":null}));
            if p["kind"] != "paid" && text(actor, "role") != "admin" {
                continue;
            }
            let c=store::optional(&mut db,"SELECT * FROM cycles WHERE account_id=? AND status!='cancelled' ORDER BY (status='open') DESC,start DESC,id DESC LIMIT 1",&[a["id"].clone()]).await?;
            let mut row = json!({"id":a["id"],"name":a["name"],"platform":a["platform"],"kind":p["kind"].as_str().unwrap_or("unconfigured"),"deleted":flag(&a,"deleted"),"profile_version":p["profile_version"],"cycle":c});
            if row["cycle"]["status"] == "open" {
                row["cycle"]["name"] = a["name"].clone();
            }
            if let Some(c) = c {
                let calc = if c["status"] == "settled" {
                    let s = one(
                        &mut db,
                        "SELECT * FROM settlements WHERE id=?",
                        &[c["settlement_id"].clone()],
                    )
                    .await?;
                    row["drift"] = json!(flag(&s, "drift"));
                    serde_json::from_str(text(&s, "snapshot"))?
                } else {
                    let now = time::now();
                    let end = c["bill_end"].as_str().or(c["end"].as_str()).unwrap_or(&now);
                    row["data_error"] = if !sync_error.is_null() {
                        sync_error.clone()
                    } else if c["cost_source"] != "upstream_bill" && c["vendor_period_id"].is_null()
                    {
                        json!(Self::coverage_error(&mut db, &c, end).await?)
                    } else {
                        Value::Null
                    };
                    if let Some(pid) = c["vendor_period_id"].as_i64() {
                        if let std::collections::btree_map::Entry::Vacant(entry) =
                            vendor_cache.entry(pid)
                        {
                            entry.insert(
                                self.vendor_calculation(&mut db, pid, end, None, false)
                                    .await?,
                            );
                        }
                        array(&vendor_cache[&pid]["calculations"])
                            .iter()
                            .find(|v| v["cycle"]["id"] == c["id"])
                            .cloned()
                            .ok_or_else(Error::unavailable)?
                    } else {
                        self.overview_calculation(&mut db, &c, end).await?
                    }
                };
                row = merge(row, start_meta(&c));
                if !calc["vendor_period"].is_null() {
                    row["vendor_period"] = calc["vendor_period"].clone();
                    row["allocation_pending"] = calc["allocation_pending"].clone();
                    row["cycle"]["cost_fen"] = calc["cycle"]["cost_fen"].clone();
                }
                row["can_settle"] = calc.get("can_settle").cloned().unwrap_or(json!(true));
                row["cost_warning"] = calc["cost_warning"].clone();
                row["mine"] = array(&calc["allocations"])
                    .iter()
                    .find(|s| s["user_id"] == actor["id"])
                    .cloned()
                    .unwrap_or(Value::Null);
                row["total_consumption"] = calc["total_consumption"].clone();
                row["creditor_name"] = json!(name(&mut db, int(&c, "creditor_id")).await?);
                row["record_count"] = calc["record_count"].clone();
                if row.get("data_error").is_some_and(|e| !e.is_null()) {
                    for key in ["total_consumption", "mine", "record_count"] {
                        row[key] = Value::Null;
                    }
                }
            }
            rows.push(row);
        }
        let balances = Self::balances(&mut db, actor, "mine").await?;
        let mut estimated: i64 = rows
            .iter()
            .filter(|r| {
                !r["mine"].is_null()
                    && r["cycle"]["status"] == "open"
                    && r["cycle"]["creditor_id"] != actor["id"]
            })
            .map(|r| int(&r["mine"], "amount_fen"))
            .sum();
        let mut vendor_totals = Vec::new();
        let mut vendor_pending = false;
        for p in all(
            &mut db,
            "SELECT * FROM vendor_periods WHERE status='open' ORDER BY id",
            &[],
        )
        .await?
        {
            let pid = int(&p, "id");
            if let std::collections::btree_map::Entry::Vacant(entry) = vendor_cache.entry(pid) {
                let now = time::now();
                let end = p["bill_end"].as_str().or(p["end"].as_str()).unwrap_or(&now);
                entry.insert(
                    self.vendor_calculation(&mut db, pid, end, None, false)
                        .await?,
                );
            }
            let mut members = Vec::new();
            let mut mine_fen = 0;
            let mut group_pending = false;
            for calc in array(&vendor_cache[&pid]["calculations"]) {
                let pending = flag(calc, "allocation_pending") || !sync_error.is_null();
                vendor_pending |= pending;
                group_pending |= pending;
                let own = array(&calc["allocations"])
                    .iter()
                    .find(|a| a["user_id"] == actor["id"]);
                let amount = own.map_or(0, |a| int(a, "amount_fen"));
                mine_fen += amount;
                if !rows.iter().any(|r| r["cycle"]["id"] == calc["cycle"]["id"])
                    && own.is_some_and(|a| !flag(a, "self_covered"))
                {
                    estimated += amount;
                }
                members.push(json!({"cycle_id":calc["cycle"]["id"],"amount_fen":if pending{Value::Null}else{json!(amount)},"allocation_pending":pending}));
            }
            vendor_totals.push(json!({"id":pid,"mine_fen":if group_pending{Value::Null}else{json!(mine_fen)},"members":members}));
        }
        for expense in all(&mut db, "SELECT * FROM expenses WHERE status='open'", &[]).await? {
            let calc = self.expense_calculation(&mut db, &expense).await?;
            estimated += array(&calc["allocations"])
                .iter()
                .filter(|s| s["user_id"] == actor["id"] && !flag(s, "self_covered"))
                .map(|s| int(s, "amount_fen"))
                .sum::<i64>();
        }
        let row_error = rows
            .iter()
            .find_map(|r| r.get("data_error").filter(|v| !v.is_null()))
            .cloned();
        let submitted=one(&mut db,"SELECT coalesce(sum(amount_fen),0) AS amount FROM orders WHERE payer_id=? AND status='submitted'",&[actor["id"].clone()]).await?;
        let mut response = json!({"accounts":rows,"account_revision":state["revision"],"account_sync_error":state["error"],"estimated_fen":if !sync_error.is_null()||row_error.is_some()||vendor_pending||rows.iter().any(|r|flag(r,"allocation_pending")){None}else{Some(estimated)},"pending_fen":balances.iter().filter(|b|b["payer_id"]==actor["id"]).map(|b|int(b,"amount_fen")).sum::<i64>(),"receivable_fen":balances.iter().filter(|b|b["creditor_id"]==actor["id"]).map(|b|int(b,"amount_fen")).sum::<i64>(),"submitted_fen":submitted["amount"],"last_sync":last,"sync_error":if sync_error.is_null(){row_error.unwrap_or(Value::Null)}else{sync_error},"coverage_from":store::meta(&mut db,"coverage_from").await?});
        if !vendor_totals.is_empty() {
            response["vendor_totals"] = json!(vendor_totals);
        }
        Ok(response)
    }
    pub async fn expenses(&self, actor: &Value, eid: Option<i64>) -> Result<Value> {
        let mut db = self.store.read().await?;
        let rows = if let Some(id) = eid {
            vec![one(&mut db, "SELECT * FROM expenses WHERE id=?", &[json!(id)]).await?]
        } else {
            all(&mut db, "SELECT * FROM expenses ORDER BY id DESC", &[]).await?
        };
        let mut items = Vec::new();
        for e in rows {
            let members: Value = serde_json::from_str(text(&e, "members"))?;
            if text(actor, "role") != "admin"
                && !array(&members).iter().any(|m| m["user_id"] == actor["id"])
            {
                continue;
            }
            let result = if e["settlement_id"].is_null() {
                self.expense_calculation(&mut db, &e).await?
            } else {
                let s = one(
                    &mut db,
                    "SELECT snapshot FROM settlements WHERE id=?",
                    &[e["settlement_id"].clone()],
                )
                .await?;
                let mut result: Value = serde_json::from_str(text(&s, "snapshot"))?;
                for key in ["status", "settlement_id", "version"] {
                    result["expense"][key] = e[key].clone();
                }
                result
            };
            items.push(Self::redact(result, actor));
        }
        if eid.is_some() {
            items
                .into_iter()
                .next()
                .ok_or_else(|| Error::new(404, "费用不存在"))
        } else {
            Ok(json!({"expenses":items}))
        }
    }
    pub async fn orders(
        &self,
        actor: &Value,
        scope: &str,
        view: &str,
        status: Option<&str>,
    ) -> Result<Value> {
        let (where_, args) = match scope {
            "all" => {
                admin(actor)?;
                ("1=1", vec![])
            }
            "receivable" => ("o.creditor_id=?", vec![actor["id"].clone()]),
            _ => ("o.payer_id=?", vec![actor["id"].clone()]),
        };
        let mut db = self.store.read().await?;
        let sql = if view == "compact" {
            if scope != "receivable" {
                return Err(Error::invalid("紧凑订单视图仅适用于收款列表"));
            }
            if status.is_some_and(|s| s != "pending") {
                return Err(Error::invalid("收款列表仅支持待付款订单"));
            }
            format!(
                "SELECT o.id,o.batch_id,o.account_name AS source_name,o.payer_name,o.amount_fen,o.status FROM orders o WHERE {where_} AND o.status='pending' ORDER BY o.id DESC"
            )
        } else {
            format!(
                "SELECT o.*,c.start,c.end,e.incurred_on,s.drift,CASE WHEN o.expense_id IS NULL THEN 'account_cycle' ELSE 'fixed_expense' END AS source_type FROM orders o LEFT JOIN cycles c ON c.id=o.cycle_id LEFT JOIN expenses e ON e.id=o.expense_id JOIN settlements s ON s.id=o.settlement_id WHERE {where_} ORDER BY o.id DESC"
            )
        };
        Ok(json!({"orders":all(&mut db,&sql,&args).await?}))
    }
    pub async fn detail(&self, actor: &Value, oid: i64) -> Result<Value> {
        let mut db = self.store.read().await?;
        let o = one(&mut db, "SELECT * FROM orders WHERE id=?", &[json!(oid)]).await?;
        if text(actor, "role") != "admin"
            && actor["id"] != o["payer_id"]
            && actor["id"] != o["creditor_id"]
        {
            return Err(Error::new(404, "订单不存在"));
        }
        let s = one(
            &mut db,
            "SELECT * FROM settlements WHERE id=?",
            &[o["settlement_id"].clone()],
        )
        .await?;
        let snapshot = self
            .present_calculation(
                Self::redact(serde_json::from_str(text(&s, "snapshot"))?, actor),
                true,
            )
            .await;
        Ok(json!({"order":o,"snapshot":snapshot,"drift":flag(&s,"drift")}))
    }
    pub async fn netting_payments(
        &self,
        actor: &Value,
        scope: &str,
        pid: Option<i64>,
    ) -> Result<Value> {
        if scope == "all" {
            admin(actor)?;
        }
        let mut db = self.store.read().await?;
        let rows = if let Some(pid) = pid {
            let p = one(
                &mut db,
                "SELECT * FROM netting_payments WHERE id=?",
                &[json!(pid)],
            )
            .await?;
            authorize(actor, int(&p, "user_a"), int(&p, "user_b"))?;
            vec![p]
        } else if scope == "all" {
            all(
                &mut db,
                "SELECT * FROM netting_payments ORDER BY id DESC",
                &[],
            )
            .await?
        } else {
            all(
                &mut db,
                "SELECT * FROM netting_payments WHERE user_a=? OR user_b=? ORDER BY id DESC",
                &[actor["id"].clone(), actor["id"].clone()],
            )
            .await?
        };
        let mut items = Vec::new();
        for p in rows {
            let mut s: Value = serde_json::from_str(text(&p, "snapshot"))?;
            if let Some(note) = optional(
                &mut db,
                "SELECT note,reference FROM rust_payment_notes WHERE payment_id=?",
                &[p["id"].clone()],
            )
            .await?
            {
                s = merge(s, note);
            }
            if pid.is_none() {
                s = without(s, &["orders"]);
            }
            items.push(merge(
                s,
                json!({"id":p["id"],"actor_id":p["actor_id"],"created_at":p["created_at"]}),
            ));
        }
        Ok(if pid.is_some() {
            items.remove(0)
        } else {
            json!({"payments":items})
        })
    }
    pub async fn history(&self, actor: &Value) -> Result<Value> {
        admin(actor)?;
        let state = self.inventory.refresh(false).await;
        let active = array(&state["active_ids"]);
        let mut db = self.store.read().await?;
        let mut cycles = Vec::new();
        let mut vendor_cache = BTreeMap::new();
        for mut c in all(&mut db,"SELECT c.*,s.drift FROM cycles c JOIN accounts a ON a.id=c.account_id AND a.deleted=0 LEFT JOIN settlements s ON s.id=c.settlement_id WHERE c.status!='cancelled' ORDER BY c.id DESC",&[]).await? {
            if !active.contains(&c["account_id"]) { continue; }
            if c["status"] == "open" && let Some(pid) = c["vendor_period_id"].as_i64() {
                if let std::collections::btree_map::Entry::Vacant(entry) = vendor_cache.entry(pid) {
                    let now = time::now();
                    let end = c["bill_end"].as_str().or(c["end"].as_str()).unwrap_or(&now);
                    entry.insert(self.vendor_calculation(&mut db, pid, end, None, false).await?);
                }
                let calc = array(&vendor_cache[&pid]["calculations"]).iter().find(|r|r["cycle"]["id"]==c["id"]).ok_or_else(Error::unavailable)?;
                c["cost_fen"] = calc["cycle"]["cost_fen"].clone();
                c["allocation_pending"] = calc["allocation_pending"].clone();
            }
            c["creditor_name"]=json!(name(&mut db,int(&c,"creditor_id")).await?);
            self.present_cycle(&mut c).await;
            cycles.push(c);
        }
        let mut audit = all(
            &mut db,
            "SELECT * FROM audit ORDER BY id DESC LIMIT 100",
            &[],
        )
        .await?;
        for a in &mut audit {
            a["actor_name"] = json!(name(&mut db, int(a, "actor_id")).await?);
            a["payload"] = serde_json::from_str(text(a, "payload"))?;
        }
        Ok(json!({"cycles":cycles,"account_revision":state["revision"],"audit":audit}))
    }
    pub async fn cycle_detail(&self, actor: &Value, cid: i64) -> Result<Value> {
        let mut db = self.store.read().await?;
        let c = one(&mut db, "SELECT * FROM cycles WHERE id=?", &[json!(cid)]).await?;
        let result = if !c["settlement_id"].is_null() {
            let s = one(
                &mut db,
                "SELECT * FROM settlements WHERE id=?",
                &[c["settlement_id"].clone()],
            )
            .await?;
            let mut r: Value = serde_json::from_str(text(&s, "snapshot"))?;
            r["drift"] = json!(flag(&s, "drift"));
            r
        } else {
            Self::fresh(&mut db).await?;
            let now = time::now();
            let end = c["bill_end"].as_str().or(c["end"].as_str()).unwrap_or(&now);
            let gap = Self::coverage_error(&mut db, &c, end).await?;
            if let Some(gap) = &gap
                && !estimate(text(&c, "start_mode"))
                && c["cost_source"] != "upstream_bill"
                && c["cost_kind"] != "estimated"
                && c["vendor_period_id"].is_null()
            {
                return Err(Error::conflict(gap));
            }
            let mut r = self.preview_calculation(&mut db, &c, end).await?;
            if let Some(gap) = gap
                && c["cost_source"] != "upstream_bill"
                && c["vendor_period_id"].is_null()
            {
                r["data_quality"] = json!("incomplete");
                r["can_settle"] = json!(false);
                r["start_warning"] = json!(gap);
            }
            r
        };
        Ok(self
            .present_calculation(Self::redact(result, actor), !c["settlement_id"].is_null())
            .await)
    }
    pub async fn inbox(&self, actor: &Value) -> Result<Value> {
        let mut db = self.store.read().await?;
        let count = one(
            &mut db,
            "SELECT count(*) AS count FROM notifications WHERE user_id=? AND read=0",
            &[actor["id"].clone()],
        )
        .await?;
        Ok(
            json!({"items":all(&mut db,"SELECT * FROM notifications WHERE user_id=? ORDER BY id DESC LIMIT 100",&[actor["id"].clone()]).await?,"unread":count["count"]}),
        )
    }
}
