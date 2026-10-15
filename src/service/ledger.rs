use super::*;
use crate::{
    json::{array, flag, merge, without},
    service::cycles::start_meta,
    store::{all, exec, optional},
};
use std::collections::BTreeSet;

pub fn authorize(actor: &Value, a: i64, b: i64) -> Result<()> {
    if a >= b || a < 1 {
        return Err(Error::invalid("请选择两个不同用户，按用户编号升序提交"));
    }
    if text(actor, "role") != "admin" && int(actor, "id") != a && int(actor, "id") != b {
        return Err(Error::new(404, "对冲账目不存在"));
    }
    Ok(())
}
pub fn order_details(rows: &[Value]) -> Value {
    json!(
        rows.iter()
            .map(|r| {
                let mut v = json!({});
                for k in [
                    "id",
                    "settlement_id",
                    "cycle_id",
                    "expense_id",
                    "payer_id",
                    "creditor_id",
                    "payer_name",
                    "creditor_name",
                    "account_name",
                    "amount_fen",
                    "status",
                    "batch_id",
                    "version",
                    "created_at",
                ] {
                    v[k] = r[k].clone();
                }
                v
            })
            .collect::<Vec<_>>()
    )
}
impl Service {
    pub async fn pending(
        db: &mut Db,
        pair: Option<(i64, i64)>,
        uid: Option<i64>,
    ) -> Result<Vec<Value>> {
        let (extra, args) = if let Some((a, b)) = pair {
            (
                " AND ((payer_id=? AND creditor_id=?) OR (payer_id=? AND creditor_id=?))",
                vec![json!(a), json!(b), json!(b), json!(a)],
            )
        } else if let Some(uid) = uid {
            (
                " AND (payer_id=? OR creditor_id=?)",
                vec![json!(uid), json!(uid)],
            )
        } else {
            ("", vec![])
        };
        all(
            db,
            &format!("SELECT * FROM orders WHERE status='pending'{extra} ORDER BY id"),
            &args,
        )
        .await
    }
    pub async fn balance(db: &mut Db, rows: &[Value]) -> Result<Value> {
        let first = rows
            .first()
            .ok_or_else(|| Error::conflict("待付款账目已更新，请刷新"))?;
        let (a, b) = (
            int(first, "payer_id").min(int(first, "creditor_id")),
            int(first, "payer_id").max(int(first, "creditor_id")),
        );
        let mut ab = 0_i64;
        let mut ba = 0_i64;
        let mut names = BTreeMap::new();
        for r in rows {
            let amount = int(r, "amount_fen");
            let sum = if int(r, "payer_id") == a {
                &mut ab
            } else {
                &mut ba
            };
            *sum = sum
                .checked_add(amount)
                .ok_or_else(|| Error::invalid("账款金额超出范围"))?;
            names.insert(int(r, "payer_id"), r["payer_name"].clone());
            names.insert(int(r, "creditor_id"), r["creditor_name"].clone());
        }
        for u in all(
            db,
            "SELECT id,name FROM users WHERE id IN (?,?)",
            &[json!(a), json!(b)],
        )
        .await?
        {
            names.insert(int(&u, "id"), u["name"].clone());
        }
        Ok(
            json!({"user_a":a,"user_b":b,"name_a":names[&a],"name_b":names[&b],"a_to_b_fen":ab,"b_to_a_fen":ba,"offset_fen":ab.min(ba),"amount_fen":(ab-ba).abs(),"payer_id":if ab>ba{Some(a)}else if ba>ab{Some(b)}else{None},"creditor_id":if ab>ba{Some(b)}else if ba>ab{Some(a)}else{None},"order_count":rows.len(),"digest":digest(&json!(rows))}),
        )
    }
    pub async fn balances(db: &mut Db, actor: &Value, scope: &str) -> Result<Vec<Value>> {
        if scope == "all" && text(actor, "role") != "admin" {
            return Err(Error::new(403, "仅管理员可查看全部对冲账目"));
        }
        let mut pairs: BTreeMap<(i64, i64), Vec<Value>> = BTreeMap::new();
        for r in Self::pending(
            db,
            None,
            if scope == "all" {
                None
            } else {
                Some(int(actor, "id"))
            },
        )
        .await?
        {
            let (a, b) = (int(&r, "payer_id"), int(&r, "creditor_id"));
            pairs.entry((a.min(b), a.max(b))).or_default().push(r);
        }
        let mut balances = Vec::new();
        for rows in pairs.values() {
            let b = Self::balance(db, rows).await?;
            if scope != "receivable"
                || (b["creditor_id"] == actor["id"] && int(&b, "amount_fen") > 0)
            {
                balances.push(b);
            }
        }
        Ok(balances)
    }
    pub async fn netting_balance(&self, actor: &Value, a: i64, b: i64) -> Result<Value> {
        authorize(actor, a, b)?;
        let mut db = self.store.read().await?;
        let rows = Self::pending(&mut db, Some((a, b)), None).await?;
        Ok(merge(
            Self::balance(&mut db, &rows).await?,
            json!({"orders":order_details(&rows)}),
        ))
    }
    pub async fn pay_netting(&self, db: &mut Db, actor: &Value, data: &Value) -> Result<Value> {
        let (a, b) = (int(data, "user_a"), int(data, "user_b"));
        authorize(actor, a, b)?;
        let rows = Self::pending(db, Some((a, b)), None).await?;
        if rows.is_empty() {
            return Err(Error::conflict("待付款账目已更新，请刷新后重新确认"));
        }
        let current = Self::balance(db, &rows).await?;
        if current["digest"] != data["digest"] {
            return Err(Error::conflict(
                "待付款账目已变化，请刷新后按最新净额重新确认",
            ));
        }
        let nonzero = int(&current, "amount_fen") != 0;
        if nonzero && text(actor, "role") != "admin" && actor["id"] != current["payer_id"] {
            return Err(Error::new(403, "仅净付款人可确认转账"));
        }
        let stamp = time::now();
        let snapshot = merge(current.clone(), json!({"orders":order_details(&rows)}));
        let pid=exec(db,"INSERT INTO netting_payments(user_a,user_b,payer_id,creditor_id,amount_fen,offset_fen,a_to_b_fen,b_to_a_fen,actor_id,created_at,digest,snapshot) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",&[json!(a),json!(b),current["payer_id"].clone(),current["creditor_id"].clone(),current["amount_fen"].clone(),current["offset_fen"].clone(),current["a_to_b_fen"].clone(),current["b_to_a_fen"].clone(),actor["id"].clone(),json!(stamp),current["digest"].clone(),json!(dumps(&snapshot))]).await?.last_insert_rowid();
        if !text(data, "note").is_empty() || !text(data, "reference").is_empty() {
            exec(db,"INSERT INTO rust_payment_notes(payment_id,note,reference,created_by,created_at) VALUES(?,?,?,?,?)",&[json!(pid),json!(text(data,"note")),json!(text(data,"reference")),actor["id"].clone(),json!(stamp)]).await?;
        }
        for r in &rows {
            let submitted = if nonzero && r["payer_id"] == current["payer_id"] {
                json!(stamp)
            } else {
                r["submitted_at"].clone()
            };
            exec(db,"UPDATE orders SET status='paid',version=version+1,submitted_at=?,paid_at=?,netting_payment_id=? WHERE id=?",&[submitted,json!(stamp),json!(pid),r["id"].clone()]).await?;
        }
        for bid in rows
            .iter()
            .map(|r| int(r, "batch_id"))
            .filter(|b| *b != 0)
            .collect::<BTreeSet<_>>()
        {
            if optional(
                db,
                "SELECT 1 AS found FROM orders WHERE batch_id=? AND status!='paid'",
                &[json!(bid)],
            )
            .await?
            .is_none()
            {
                exec(
                    db,
                    "UPDATE batches SET status='paid' WHERE id=?",
                    &[json!(bid)],
                )
                .await?;
            }
        }
        let title = format!(
            "{}{}",
            if text(actor, "role") == "admin" && actor["id"] != current["payer_id"] {
                "管理员已标记"
            } else {
                ""
            },
            if nonzero {
                "转账结清"
            } else {
                "对冲结清"
            }
        );
        for uid in [a, b] {
            notify(db, uid, &title, Value::Null).await?;
        }
        audit(
            db,
            actor,
            "netting_payment",
            json!(pid),
            &merge(
                current.clone(),
                json!({"orders":rows.iter().map(|r|r["id"].clone()).collect::<Vec<_>>()}),
            ),
        )
        .await?;
        Ok(
            json!({"id":pid,"amount_fen":current["amount_fen"],"offset_fen":current["offset_fen"],"created_at":stamp}),
        )
    }
    pub async fn prepare_settlement(&self, actor: &Value, data: &Value) -> Result<Prepared> {
        admin(actor)?;
        if data.get("delete_scopes").is_some()
            && !self.inventory.refresh(true).await["error"].is_null()
        {
            return Err(Error::new(503, "无法核验删除范围，请稍后重试"));
        }
        let mut db = self.store.read().await?;
        let p = store::one(
            &mut db,
            "SELECT * FROM previews WHERE id=? AND actor_id=?",
            &[data["preview_id"].clone(), actor["id"].clone()],
            "结算预览不存在",
        )
        .await?;
        if text(&p, "expires_at") < time::now().as_str() {
            return Err(Error::conflict("预览已过期，请重新预览"));
        }
        let snaps: Value = serde_json::from_str(text(&p, "payload"))?;
        drop(db);
        let cycles: Vec<_> = array(&snaps)
            .iter()
            .filter(|s| s.get("cycle").is_some())
            .collect();
        if !cycles.is_empty() {
            let ids = cycles
                .iter()
                .map(|s| s["cycle"]["id"].clone())
                .collect::<Vec<_>>();
            let end = cycles.iter().map(|s| text(s, "end")).max().unwrap();
            self.sync_selection(&ids, end, None).await?;
        }
        let mut db = self.store.read().await?;
        let mut prepared = Prepared::default();
        for snap in cycles {
            let c = one(
                &mut db,
                "SELECT * FROM cycles WHERE id=?",
                &[snap["cycle"]["id"].clone()],
            )
            .await?;
            let revision = store::revision(&mut db, int(&c, "account_id")).await?;
            prepared.calculations.insert(
                int(&c, "id"),
                (
                    self.calculation(&mut db, &c, text(snap, "end")).await?,
                    revision,
                ),
            );
        }
        for snap in array(&snaps)
            .iter()
            .filter(|s| s["source_type"] == "fixed_expense")
        {
            let expense = one(
                &mut db,
                "SELECT * FROM expenses WHERE id=?",
                &[snap["expense"]["id"].clone()],
            )
            .await?;
            prepared.expenses.insert(
                int(&expense, "id"),
                self.expense_calculation(&mut db, &expense).await?,
            );
        }
        Ok(prepared)
    }
    pub async fn settle(
        &self,
        db: &mut Db,
        actor: &Value,
        data: &Value,
        mut prepared: Prepared,
        session_id: Option<&str>,
    ) -> Result<Value> {
        admin(actor)?;
        let p = store::one(
            db,
            "SELECT * FROM previews WHERE id=? AND actor_id=?",
            &[data["preview_id"].clone(), actor["id"].clone()],
            "结算预览不存在",
        )
        .await?;
        if text(&p, "expires_at") < time::now().as_str() {
            return Err(Error::conflict("预览已过期，请重新预览"));
        }
        let snaps: Value = serde_json::from_str(text(&p, "payload"))?;
        let cycle_ids: BTreeSet<_> = array(&snaps)
            .iter()
            .filter(|s| s.get("cycle").is_some())
            .map(|s| int(&s["cycle"], "id"))
            .collect();
        if !cycle_ids.is_empty() {
            Self::fresh(db).await?;
        }
        let renewals: BTreeMap<_, _> = array(&data["renewals"])
            .iter()
            .map(|r| (int(r, "cycle_id"), r))
            .collect();
        let deletes: BTreeSet<_> = array(&data["delete_cycle_ids"])
            .iter()
            .filter_map(Value::as_i64)
            .collect();
        if !deletes.is_subset(&cycle_ids) || deletes.iter().any(|id| renewals.contains_key(id)) {
            return Err(Error::invalid("删除账号必须属于本次结算，且不能同时续期"));
        }
        if renewals.keys().any(|id| !cycle_ids.contains(id)) {
            return Err(Error::invalid("续期不属于本次结算"));
        }
        let mut results = Vec::new();
        let mut jobs = Vec::new();
        for snap in array(&snaps) {
            if snap["source_type"] == "fixed_expense" {
                let mut e = one(
                    db,
                    "SELECT * FROM expenses WHERE id=?",
                    &[snap["expense"]["id"].clone()],
                )
                .await?;
                if e["status"] != "open" || e["version"] != snap["expense"]["version"] {
                    return Err(Error::conflict("固定费用已修改或已出单，请重新预览"));
                }
                if let Some(members) = e["members"].as_str() {
                    e["members"] = serde_json::from_str(members)?;
                }
                let calc = prepared
                    .expenses
                    .remove(&int(&e, "id"))
                    .ok_or_else(Error::unavailable)?;
                if calc["digest"] != snap["digest"] || calc["digest"] != digest(&e) {
                    return Err(Error::conflict("固定费用分摊已变化，请重新预览"));
                }
                let sid=exec(db,"INSERT INTO settlements(expense_id,snapshot,digest,created_at) VALUES(?,?,?,?)",&[e["id"].clone(),json!(dumps(&calc)),calc["digest"].clone(),json!(time::now())]).await?.last_insert_rowid();
                exec(db,"UPDATE expenses SET status='settled',settlement_id=?,version=version+1 WHERE id=?",&[json!(sid),e["id"].clone()]).await?;
                Self::create_orders(db, sid, &calc["allocations"], &e, true).await?;
                audit(
                    db,
                    actor,
                    "settle_expense",
                    e["id"].clone(),
                    &json!({"settlement_id":sid,"cost_fen":e["cost_fen"]}),
                )
                .await?;
                results.push(json!({"expense_id":e["id"],"settlement_id":sid}));
                continue;
            }
            let cid = int(&snap["cycle"], "id");
            let c = one(db, "SELECT * FROM cycles WHERE id=?", &[json!(cid)]).await?;
            if c["status"] != "open" || c["version"] != snap["cycle"]["version"] {
                return Err(Error::conflict("成本或周期已修改，请重新预览"));
            }
            for key in [
                "start_mode",
                "start_lower",
                "start_upper",
                "start_confirmed",
            ] {
                if start_meta(&c)[key] != start_meta(&snap["cycle"])[key] {
                    return Err(Error::conflict("周期起点已修改，请重新预览"));
                }
            }
            if snap.get("can_settle").is_some_and(|v| v == false) || !flag(&c, "start_confirmed") {
                return Err(Error::conflict(
                    snap["cost_warning"]
                        .as_str()
                        .unwrap_or("请先确认周期起点并重新预览"),
                ));
            }
            if let Some(gap) = Self::coverage_error(db, &c, text(snap, "end")).await?
                && c["cost_source"] != "upstream_bill"
            {
                return Err(Error::conflict(gap));
            }
            let (mut calc, rev) = prepared
                .calculations
                .remove(&cid)
                .ok_or_else(Error::unavailable)?;
            if !flag(&calc, "can_settle") {
                return Err(Error::conflict(
                    calc["cost_warning"].as_str().unwrap_or("请确认周期起点"),
                ));
            }
            if rev != store::revision(db, int(&c, "account_id")).await?
                || calc["cycle"]["version"] != c["version"]
                || calc["digest"] != snap["digest"]
            {
                return Err(Error::conflict("消费记录已更新，请重新预览"));
            }
            let records = calc["records"].take();
            let calc = without(calc, &["records"]);
            let sid = exec(
                db,
                "INSERT INTO settlements(cycle_id,snapshot,digest,created_at) VALUES(?,?,?,?)",
                &[
                    json!(cid),
                    json!(dumps(&calc)),
                    calc["digest"].clone(),
                    json!(time::now()),
                ],
            )
            .await?
            .last_insert_rowid();
            for chunk in array(&records).chunks(256) {
                let mut q = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                    "INSERT INTO settlement_usage(settlement_id,usage_id,payload) ",
                );
                q.push_values(chunk, |mut b, r| {
                    b.push_bind(sid).push_bind(int(r, "id")).push_bind(dumps(r));
                });
                q.build().execute(&mut *db).await?;
            }
            exec(db,"UPDATE cycles SET status='settled',end=?,settlement_id=?,version=version+1 WHERE id=?",&[snap["end"].clone(),json!(sid),json!(cid)]).await?;
            Self::create_orders(db, sid, &calc["allocations"], &c, false).await?;
            audit(
                db,
                actor,
                "settle",
                json!(cid),
                &json!({"settlement_id":sid,"cost_fen":c["cost_fen"]}),
            )
            .await?;
            let mut result = json!({"cycle_id":cid,"settlement_id":sid});
            if deletes.contains(&cid) {
                Self::assert_not_deleting(db, int(&c, "account_id")).await?;
                let a = one(
                    db,
                    "SELECT * FROM accounts WHERE id=?",
                    &[c["account_id"].clone()],
                )
                .await?;
                let stamp = time::now();
                let scope = if let Some(expected) = data
                    .get("delete_scopes")
                    .and_then(|v| v.get(cid.to_string()))
                {
                    let scope = crate::deletions::scope_from_db(db, int(&c, "account_id")).await?;
                    if scope["digest"] != *expected {
                        return Err(Error::conflict("删除范围已变化，请重新确认账号与影子账号"));
                    }
                    Some(scope)
                } else {
                    None
                };
                let jid=exec(db,"INSERT INTO account_deletions(account_id,settlement_id,account_name,account_created_at,actor_id,session_id,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?)",&[a["id"].clone(),json!(sid),c["name"].clone(),a["created_at"].clone(),actor["id"].clone(),json!(session_id),json!(stamp),json!(stamp)]).await?.last_insert_rowid();
                if let Some(scope) = scope {
                    exec(db,"INSERT INTO rust_deletion_scopes(deletion_id,digest,snapshot) VALUES(?,?,?)",&[json!(jid),scope["digest"].clone(),json!(dumps(&scope))]).await?;
                }
                jobs.push(json!({"id":jid,"account_id":a["id"],"status":"queued"}));
                audit(
                    db,
                    actor,
                    "request_account_deletion",
                    a["id"].clone(),
                    &json!({"deletion_id":jid,"settlement_id":sid}),
                )
                .await?;
            }
            if let Some(r) = renewals.get(&cid) {
                let kind =
                    r["cost_kind"]
                        .as_str()
                        .unwrap_or(if c["cost_source"] == "upstream_bill" {
                            "estimated"
                        } else {
                            c["cost_kind"].as_str().unwrap_or("exact")
                        });
                let next=self.create_cycle(db,actor,&json!({"account_id":c["account_id"],"name":c["name"],"creditor_id":r["creditor_id"],"cost":r["cost"],"cost_kind":kind,"start":snap["end"],"start_mode":"exact","renewed_from_cycle_id":cid})).await?;
                result["next_cycle"] = next["id"].clone();
            }
            results.push(result);
        }
        let mut result = json!({"settlements":results});
        if !jobs.is_empty() {
            result["deletions"] = json!(jobs);
        }
        Ok(result)
    }
    async fn create_orders(
        db: &mut Db,
        sid: i64,
        shares: &Value,
        source: &Value,
        expense: bool,
    ) -> Result<()> {
        let creditor = name(db, int(source, "creditor_id")).await?;
        for s in array(shares) {
            if flag(s, "self_covered") || int(s, "amount_fen") == 0 {
                continue;
            }
            let oid=exec(db,"INSERT INTO orders(settlement_id,cycle_id,expense_id,payer_id,creditor_id,payer_name,creditor_name,account_name,amount_fen,created_at) VALUES(?,?,?,?,?,?,?,?,?,?)",&[json!(sid),if expense{Value::Null}else{source["id"].clone()},if expense{source["id"].clone()}else{Value::Null},s["user_id"].clone(),source["creditor_id"].clone(),s["user_name"].clone(),json!(creditor),source["name"].clone(),s["amount_fen"].clone(),json!(time::now())]).await?.last_insert_rowid();
            notify(db, int(s, "user_id"), "新结算订单", json!(oid)).await?;
            notify(db, int(source, "creditor_id"), "新增应收订单", json!(oid)).await?;
        }
        Ok(())
    }
    pub async fn merge(&self, db: &mut Db, actor: &Value, data: &Value) -> Result<Value> {
        let ids = if data.is_array() {
            array(data)
        } else {
            array(&data["ids"])
        };
        let mut rows = Vec::new();
        for id in ids {
            rows.push(
                one(
                    db,
                    "SELECT * FROM orders WHERE id=?",
                    std::slice::from_ref(id),
                )
                .await?,
            );
        }
        let first = rows.first().ok_or_else(|| Error::invalid("请选择订单"))?;
        if text(actor, "role") != "admin" && actor["id"] != first["payer_id"] {
            return Err(Error::new(403, "仅付款人可合并订单"));
        }
        if rows.iter().any(|r| {
            r["status"] != "pending"
                || !r["batch_id"].is_null()
                || r["payer_id"] != first["payer_id"]
                || r["creditor_id"] != first["creditor_id"]
        }) {
            return Err(Error::conflict(
                "只能合并同一付款人、同一收款人的未合并待付款订单",
            ));
        }
        let bid = exec(
            db,
            "INSERT INTO batches(payer_id,creditor_id,created_at) VALUES(?,?,?)",
            &[
                first["payer_id"].clone(),
                first["creditor_id"].clone(),
                json!(time::now()),
            ],
        )
        .await?
        .last_insert_rowid();
        for r in rows {
            exec(
                db,
                "UPDATE orders SET batch_id=?,version=version+1 WHERE id=?",
                &[json!(bid), r["id"].clone()],
            )
            .await?;
        }
        audit(db, actor, "merge", json!(bid), &json!({"orders":ids})).await?;
        Ok(json!({"id":bid}))
    }
    pub async fn transition(
        &self,
        db: &mut Db,
        actor: &Value,
        oid: i64,
        requested: &str,
        version: i64,
    ) -> Result<Value> {
        let action = if requested == "confirm" {
            if text(actor, "role") != "admin" {
                return Err(Error::new(403, "收款方无需确认，请由管理员处理"));
            }
            "mark_paid"
        } else {
            requested
        };
        if action == "reject" {
            return Err(Error::conflict("收款方无需退回订单"));
        }
        if !["submit", "mark_paid", "split"].contains(&action) {
            return Err(Error::invalid("订单操作无效"));
        }
        let o = one(db, "SELECT * FROM orders WHERE id=?", &[json!(oid)]).await?;
        if int(&o, "version") != version {
            return Err(Error::conflict("订单已更新，请刷新"));
        }
        let rows = if o["batch_id"].is_null() {
            vec![o.clone()]
        } else {
            all(
                db,
                "SELECT * FROM orders WHERE batch_id=?",
                &[o["batch_id"].clone()],
            )
            .await?
        };
        if text(actor, "role") != "admin" {
            if action == "mark_paid" {
                return Err(Error::new(403, "仅管理员可标记订单结清"));
            }
            if actor["id"] != o["payer_id"] {
                return Err(Error::new(403, "无权操作此订单"));
            }
        }
        if action == "split" {
            if rows.iter().any(|r| r["status"] != "pending") {
                return Err(Error::conflict("订单状态已改变"));
            }
            if o["batch_id"].is_null() {
                return Err(Error::conflict("此订单未合并"));
            }
            exec(
                db,
                "UPDATE batches SET status='split' WHERE id=?",
                &[o["batch_id"].clone()],
            )
            .await?;
            exec(
                db,
                "UPDATE orders SET batch_id=NULL,version=version+1 WHERE batch_id=?",
                &[o["batch_id"].clone()],
            )
            .await?;
        } else {
            if rows.iter().any(|r| r["status"] == "pending") {
                return Err(Error::conflict(
                    "付款方式已升级，请刷新账单，按双方对冲后的净额确认",
                ));
            }
            if action == "submit" {
                return Err(Error::conflict("订单状态已改变"));
            }
            if rows
                .iter()
                .any(|r| r["status"] != "pending" && r["status"] != "submitted")
            {
                return Err(Error::conflict("只能标记待付款或待处理订单"));
            }
            let stamp = time::now();
            for r in &rows {
                let submitted = r["submitted_at"].as_str().unwrap_or(&stamp);
                exec(db,"UPDATE orders SET status='paid',version=version+1,submitted_at=?,paid_at=? WHERE id=?",&[json!(submitted),json!(stamp),r["id"].clone()]).await?;
                notify(
                    db,
                    int(r, "payer_id"),
                    "管理员已将订单标记为已结清",
                    r["id"].clone(),
                )
                .await?;
            }
        }
        audit(db,actor,action,json!(oid),&json!({"orders":rows.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"batch_id":o["batch_id"],"requested_action":requested})).await?;
        Ok(json!({"ok":true}))
    }
    pub async fn void(&self, db: &mut Db, actor: &Value, sid: i64) -> Result<Value> {
        admin(actor)?;
        if optional(db,"SELECT 1 AS found FROM account_deletions WHERE settlement_id=? AND status IN ('queued','running','verify')",&[json!(sid)]).await?.is_some(){return Err(Error::conflict("账号删除处理中，暂不能作废结算"));}
        let s = one(db, "SELECT * FROM settlements WHERE id=?", &[json!(sid)]).await?;
        if s["status"] != "active" {
            return Err(Error::conflict("结算已作废"));
        }
        let rows = all(
            db,
            "SELECT * FROM orders WHERE settlement_id=?",
            &[json!(sid)],
        )
        .await?;
        if rows.iter().any(|o| o["status"] != "pending") {
            return Err(Error::conflict("已有报已付或结清订单，不能作废"));
        }
        for bid in rows
            .iter()
            .map(|r| int(r, "batch_id"))
            .filter(|b| *b != 0)
            .collect::<BTreeSet<_>>()
        {
            exec(
                db,
                "UPDATE batches SET status='split' WHERE id=?",
                &[json!(bid)],
            )
            .await?;
            exec(
                db,
                "UPDATE orders SET batch_id=NULL,version=version+1 WHERE batch_id=?",
                &[json!(bid)],
            )
            .await?;
        }
        exec(
            db,
            "UPDATE orders SET status='void',version=version+1 WHERE settlement_id=?",
            &[json!(sid)],
        )
        .await?;
        exec(
            db,
            "UPDATE settlements SET status='void' WHERE id=?",
            &[json!(sid)],
        )
        .await?;
        let (table, id, action) = if s["expense_id"].is_null() {
            ("cycles", s["cycle_id"].clone(), "void")
        } else {
            ("expenses", s["expense_id"].clone(), "void_expense")
        };
        exec(
            db,
            &format!(
                "UPDATE {table} SET status='open',settlement_id=NULL,version=version+1 WHERE id=?"
            ),
            std::slice::from_ref(&id),
        )
        .await?;
        for o in rows {
            notify(db, int(&o, "payer_id"), "结算订单已作废", o["id"].clone()).await?;
        }
        audit(db, actor, action, id, &json!({"settlement_id":sid})).await?;
        Ok(json!({"ok":true}))
    }
}
