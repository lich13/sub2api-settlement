use super::*;
use crate::{
    json::{array, flag, merge, without},
    service::{
        cycles::{estimate, start_meta},
        ledger::authorize,
    },
    store::all,
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
        let earliest = all(
            &mut db,
            "SELECT root_id,MIN(created_at) AS created_at FROM usage GROUP BY root_id",
            &[],
        )
        .await?
        .into_iter()
        .map(|r| (int(&r, "root_id"), r["created_at"].clone()))
        .collect::<BTreeMap<_, _>>();
        for a in &mut accounts {
            a["earliest_usage_at"] = earliest.get(&int(a, "id")).cloned().unwrap_or(Value::Null);
        }
        Ok(
            json!({"accounts":accounts,"account_revision":state["revision"],"account_sync_error":state["error"],"users":all(&mut db,"SELECT id,name FROM users WHERE deleted=0 AND status='active' ORDER BY id",&[]).await?,"coverage_from":store::meta(&mut db,"coverage_from").await?}),
        )
    }
    pub async fn overview(&self, actor: &Value) -> Result<Value> {
        let (state, accounts) = self.inventory.active_snapshot().await;
        let mut db = self.store.read().await?;
        let mut rows = Vec::with_capacity(accounts.len());
        let mut vendor_cache = BTreeMap::<i64, Value>::new();
        let mut sync_error = store::meta(&mut db, "sync_error").await?;
        let last = store::meta(&mut db, "last_sync").await?;
        if (last.is_null()
            || time::parse(&time::now())? - time::parse(last.as_str().unwrap_or(""))?
                > time::Duration::minutes(3))
            && sync_error.is_null()
        {
            sync_error = json!("消费记录同步已中断，请稍后重试");
        }
        let names = Self::names(&mut db).await?;
        let (coverage_from, coverage_gaps) = Self::coverage_meta(&mut db).await?;
        let profiles = all(
            &mut db,
            "SELECT account_id,kind,vendor_id,version AS profile_version FROM profiles",
            &[],
        )
        .await?
        .into_iter()
        .map(|p| (int(&p, "account_id"), p))
        .collect::<BTreeMap<_, _>>();
        let mut latest_cycles = BTreeMap::<i64, Value>::new();
        for c in all(
            &mut db,
            "SELECT * FROM cycles WHERE status!='cancelled' ORDER BY (status='open') DESC,start DESC,id DESC",
            &[],
        )
        .await?
        {
            latest_cycles.entry(int(&c, "account_id")).or_insert(c);
        }
        let settled_ids: Vec<i64> = latest_cycles
            .values()
            .filter(|c| c["status"] == "settled" && !c["settlement_id"].is_null())
            .map(|c| int(c, "settlement_id"))
            .collect();
        let settlements = if settled_ids.is_empty() {
            BTreeMap::new()
        } else {
            all(
                &mut db,
                "SELECT id,drift,snapshot FROM settlements WHERE id IN (SELECT value FROM json_each(?))",
                &[Value::String(dumps(&settled_ids))],
            )
            .await?
            .into_iter()
            .map(|s| (int(&s, "id"), s))
            .collect::<BTreeMap<_, _>>()
        };
        let is_admin = text(actor, "role") == "admin";
        let default_profile = json!({"kind":null,"profile_version":null});
        let now = time::now();
        for a in accounts {
            let aid = int(&a, "id");
            let p = profiles.get(&aid).unwrap_or(&default_profile);
            if p["kind"] != "paid" && !is_admin {
                continue;
            }
            let c = latest_cycles.remove(&aid);
            let mut row = json!({"id":a["id"],"name":a["name"],"platform":a["platform"],"kind":p["kind"].as_str().unwrap_or("unconfigured"),"deleted":flag(&a,"deleted"),"profile_version":p["profile_version"],"cycle":c});
            if !p["vendor_id"].is_null() {
                row["vendor_id"] = p["vendor_id"].clone();
            }
            if row["cycle"]["status"] == "open" {
                row["cycle"]["name"] = a["name"].clone();
            }
            if let Some(c) = c {
                let calc = if c["status"] == "settled" {
                    let s = settlements
                        .get(&int(&c, "settlement_id"))
                        .ok_or_else(|| Error::new(404, "记录不存在"))?;
                    row["drift"] = json!(flag(s, "drift"));
                    serde_json::from_str(text(s, "snapshot"))?
                } else {
                    let end = c["bill_end"].as_str().or(c["end"].as_str()).unwrap_or(&now);
                    row["data_error"] = if !sync_error.is_null() {
                        sync_error.clone()
                    } else if c["cost_source"] != "upstream_bill" && c["vendor_period_id"].is_null()
                    {
                        json!(Self::coverage_error_from(
                            &coverage_from,
                            &coverage_gaps,
                            &c,
                            end
                        ))
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
                        self.overview_calculation_with_names(&mut db, &c, end, &names)
                            .await?
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
                if !c["vendor_period_id"].is_null() {
                    row["display"] = self
                        .present_calculation(calc.clone(), c["status"] == "settled")
                        .await["display"]
                        .clone();
                }
                row["total_consumption"] = calc["total_consumption"].clone();
                row["creditor_name"] = json!(Self::name_from(&names, int(&c, "creditor_id")));
                row["record_count"] = calc["record_count"].clone();
                if row.get("data_error").is_some_and(|e| !e.is_null()) {
                    if row.get("display").is_some() {
                        row["display"]["cost_multiplier"] = Value::Null;
                    }
                    for key in ["total_consumption", "mine", "record_count"] {
                        row[key] = Value::Null;
                    }
                }
            }
            rows.push(row);
        }
        let balances = Self::balances_with_names(&mut db, actor, "mine", &names).await?;
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
                let end = p["bill_end"].as_str().or(p["end"].as_str()).unwrap_or(&now);
                entry.insert(
                    self.vendor_calculation(&mut db, pid, end, None, false)
                        .await?,
                );
            }
            let mut members = Vec::new();
            for calc in array(&vendor_cache[&pid]["calculations"]) {
                let pending = flag(calc, "allocation_pending") || !sync_error.is_null();
                vendor_pending |= pending;
                let own = array(&calc["allocations"])
                    .iter()
                    .find(|a| a["user_id"] == actor["id"]);
                let amount = own.map_or(0, |a| int(a, "amount_fen"));
                if !rows.iter().any(|r| r["cycle"]["id"] == calc["cycle"]["id"])
                    && own.is_some_and(|a| !flag(a, "self_covered"))
                {
                    estimated += amount;
                }
                members.push(json!({"cycle_id":calc["cycle"]["id"],"amount_fen":if pending{Value::Null}else{json!(amount)},"allocation_pending":pending}));
            }
            vendor_totals.push(json!({"id":pid,"members":members}));
        }
        for expense in all(&mut db, "SELECT * FROM expenses WHERE status='open'", &[]).await? {
            let calc = self
                .expense_calculation_with_names(&expense, &names)
                .await?;
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
        let mut response = json!({"accounts":rows,"account_revision":state["revision"],"account_sync_error":state["error"],"estimated_fen":if !sync_error.is_null()||row_error.is_some()||vendor_pending||rows.iter().any(|r|flag(r,"allocation_pending")){None}else{Some(estimated)},"pending_fen":balances.iter().filter(|b|b["payer_id"]==actor["id"]).map(|b|int(b,"amount_fen")).sum::<i64>(),"receivable_fen":balances.iter().filter(|b|b["creditor_id"]==actor["id"]).map(|b|int(b,"amount_fen")).sum::<i64>(),"last_sync":last,"sync_error":if sync_error.is_null(){row_error.unwrap_or(Value::Null)}else{sync_error},"coverage_from":coverage_from});
        if !vendor_totals.is_empty() {
            response["vendor_totals"] = json!(vendor_totals);
        }
        Ok(response)
    }
    pub async fn expenses(&self, actor: &Value, eid: Option<i64>) -> Result<Value> {
        let mut db = self.store.read().await?;
        let rows = if let Some(id) = eid {
            vec![
                one(
                    &mut db,
                    "SELECT e.*,s.snapshot AS settlement_snapshot FROM expenses e LEFT JOIN settlements s ON s.id=e.settlement_id WHERE e.id=?",
                    &[json!(id)],
                )
                .await?,
            ]
        } else {
            all(
                &mut db,
                "SELECT e.*,s.snapshot AS settlement_snapshot FROM expenses e LEFT JOIN settlements s ON s.id=e.settlement_id ORDER BY e.id DESC",
                &[],
            )
            .await?
        };
        let mut names: Option<BTreeMap<i64, String>> = None;
        let mut items = Vec::new();
        for mut e in rows {
            let snap = e
                .as_object_mut()
                .and_then(|o| o.remove("settlement_snapshot"))
                .unwrap_or(Value::Null);
            let members: Value = serde_json::from_str(text(&e, "members"))?;
            if text(actor, "role") != "admin"
                && !array(&members).iter().any(|m| m["user_id"] == actor["id"])
            {
                continue;
            }
            let result = if e["settlement_id"].is_null() {
                if names.is_none() {
                    names = Some(Self::names(&mut db).await?);
                }
                self.expense_calculation_with_names(&e, names.as_ref().unwrap())
                    .await?
            } else {
                let s = snap.as_str().ok_or_else(|| Error::new(404, "记录不存在"))?;
                let mut result: Value = serde_json::from_str(s)?;
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
    pub async fn orders(&self, actor: &Value, scope: &str) -> Result<Value> {
        let (where_, args) = match scope {
            "all" => {
                admin(actor)?;
                ("1=1", vec![])
            }
            "receivable" => ("o.creditor_id=?", vec![actor["id"].clone()]),
            _ => ("o.payer_id=?", vec![actor["id"].clone()]),
        };
        let mut db = self.store.read().await?;
        let sql = format!(
            "SELECT o.*,c.start,c.end,e.incurred_on,s.drift,CASE WHEN o.expense_id IS NULL THEN 'account_cycle' ELSE 'fixed_expense' END AS source_type FROM orders o LEFT JOIN cycles c ON c.id=o.cycle_id LEFT JOIN expenses e ON e.id=o.expense_id JOIN settlements s ON s.id=o.settlement_id WHERE {where_} ORDER BY o.id DESC"
        );
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
                "SELECT p.*,n.payment_id AS has_note,n.note,n.reference FROM netting_payments p LEFT JOIN rust_payment_notes n ON n.payment_id=p.id WHERE p.id=?",
                &[json!(pid)],
            )
            .await?;
            authorize(actor, int(&p, "user_a"), int(&p, "user_b"))?;
            vec![p]
        } else if scope == "all" {
            all(
                &mut db,
                "SELECT p.*,n.payment_id AS has_note,n.note,n.reference FROM netting_payments p LEFT JOIN rust_payment_notes n ON n.payment_id=p.id ORDER BY p.id DESC",
                &[],
            )
            .await?
        } else {
            all(
                &mut db,
                "SELECT p.*,n.payment_id AS has_note,n.note,n.reference FROM netting_payments p LEFT JOIN rust_payment_notes n ON n.payment_id=p.id WHERE p.user_a=? OR p.user_b=? ORDER BY p.id DESC",
                &[actor["id"].clone(), actor["id"].clone()],
            )
            .await?
        };
        let mut items = Vec::with_capacity(rows.len());
        for p in rows {
            let mut s: Value = serde_json::from_str(text(&p, "snapshot"))?;
            if !p["has_note"].is_null() {
                s = merge(s, json!({"note":p["note"],"reference":p["reference"]}));
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
        let active: std::collections::BTreeSet<i64> = array(&state["active_ids"])
            .iter()
            .filter_map(Value::as_i64)
            .collect();
        let mut db = self.store.read().await?;
        let names = Self::names(&mut db).await?;
        let mut cycles = Vec::new();
        let mut vendor_cache = BTreeMap::new();
        let now = time::now();
        for mut c in all(&mut db,"SELECT c.*,s.drift FROM cycles c JOIN accounts a ON a.id=c.account_id AND a.deleted=0 LEFT JOIN settlements s ON s.id=c.settlement_id WHERE c.status!='cancelled' ORDER BY c.id DESC",&[]).await? {
            if !active.contains(&int(&c, "account_id")) { continue; }
            if c["status"] == "open" && let Some(pid) = c["vendor_period_id"].as_i64() {
                if let std::collections::btree_map::Entry::Vacant(entry) = vendor_cache.entry(pid) {
                    let end = c["bill_end"].as_str().or(c["end"].as_str()).unwrap_or(&now);
                    entry.insert(self.vendor_calculation(&mut db, pid, end, None, false).await?);
                }
                let calc = array(&vendor_cache[&pid]["calculations"]).iter().find(|r|r["cycle"]["id"]==c["id"]).ok_or_else(Error::unavailable)?;
                c["cost_fen"] = calc["cycle"]["cost_fen"].clone();
                c["allocation_pending"] = calc["allocation_pending"].clone();
            }
            c["creditor_name"] = json!(Self::name_from(&names, int(&c, "creditor_id")));
            self.present_cycle(&mut c).await;
            cycles.push(c);
        }
        Ok(json!({"cycles":cycles,"account_revision":state["revision"]}))
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
