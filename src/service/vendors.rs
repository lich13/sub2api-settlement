use super::*;
use crate::{
    json::{array, flag},
    money::cents,
    store::{all, exec, optional},
};
use std::collections::BTreeSet;

impl Service {
    pub async fn validate_vendor_settlement(
        &self,
        db: &mut Db,
        snaps: &Value,
        data: &Value,
    ) -> Result<Vec<Value>> {
        let mut periods = BTreeMap::new();
        for snap in array(snaps) {
            let Some(pid) = snap["cycle"]["vendor_period_id"].as_i64() else {
                continue;
            };
            let mut p = one(db, "SELECT * FROM vendor_periods WHERE id=?", &[json!(pid)]).await?;
            if p["status"] != "open"
                || p["version"] != snap["vendor_period"]["version"]
                || p["bill_digest"] != snap["digest"]
                || p["bill_end"] != snap["end"]
            {
                return Err(Error::conflict("供应商结算依据已变化，请重新预览"));
            }
            p["settlement_end"] = snap["end"].clone();
            periods.insert(pid, p);
        }
        for r in array(&data["vendor_renewals"]) {
            let p = periods
                .get(&int(r, "period_id"))
                .ok_or_else(|| Error::invalid("续费供应商不在本次结算中"))?;
            if p["version"] != r["version"] {
                return Err(Error::conflict("供应商续费设置已变化"));
            }
            if optional(db,"SELECT id FROM vendor_periods WHERE vendor_id=? AND status='open' AND end IS NULL AND id!=?",&[p["vendor_id"].clone(),p["id"].clone()]).await?.is_some(){return Err(Error::conflict("该供应商已有续费周期，不能重复续费"));}
            let members = Self::vendor_members(db, int(p, "id")).await?;
            if array(&r["cycle_ids"]).is_empty() {
                return Err(Error::invalid("请选择下期成员"));
            }
            for cid in array(&r["cycle_ids"]) {
                if !members
                    .iter()
                    .any(|c| c["id"] == *cid && !flag(c, "account_deleted"))
                {
                    return Err(Error::invalid("下期成员必须是本期仍存在的账号"));
                }
                if array(&data["delete_cycle_ids"]).contains(cid) {
                    return Err(Error::invalid("续费与删除不能同时选择"));
                }
            }
        }
        Ok(periods.into_values().collect())
    }
    pub async fn finish_vendor_settlement(
        &self,
        db: &mut Db,
        actor: &Value,
        periods: &[Value],
        data: &Value,
    ) -> Result<Vec<Value>> {
        let mut next = Vec::new();
        for p in periods {
            let pid = int(p, "id");
            exec(
                db,
                "UPDATE vendor_periods SET status='settled',end=?,version=version+1 WHERE id=?",
                &[p["settlement_end"].clone(), json!(pid)],
            )
            .await?;
            let members = Self::vendor_members(db, pid).await?;
            for c in &members {
                exec(db,"UPDATE profiles SET vendor_id=NULL,version=version+1 WHERE account_id=? AND NOT EXISTS(SELECT 1 FROM cycles x WHERE x.account_id=? AND x.status='open' AND x.vendor_period_id IS NOT NULL)",&[c["account_id"].clone(),c["account_id"].clone()]).await?;
            }
            if let Some(r) = array(&data["vendor_renewals"])
                .iter()
                .find(|r| r["period_id"] == p["id"])
            {
                let cost = cents(text(r, "cost"))?;
                one(
                    db,
                    "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
                    &[r["creditor_id"].clone()],
                )
                .await?;
                let new=exec(db,"INSERT INTO vendor_periods(vendor_id,name,cost_fen,creditor_id,start,renewed_from_period_id,created_at) VALUES(?,?,?,?,?,?,?)",&[p["vendor_id"].clone(),p["name"].clone(),json!(cost),r["creditor_id"].clone(),p["settlement_end"].clone(),json!(pid),json!(time::now())]).await?.last_insert_rowid();
                for cid in array(&r["cycle_ids"]) {
                    let c = members
                        .iter()
                        .find(|c| c["id"] == *cid)
                        .ok_or_else(Error::unavailable)?;
                    let created=self.create_cycle(db,actor,&json!({"account_id":c["account_id"],"name":c["current_name"],"cost":"0.00","cost_kind":"exact","creditor_id":r["creditor_id"],"start":p["settlement_end"],"start_mode":"exact","renewed_from_cycle_id":c["id"]})).await?;
                    exec(db,"UPDATE cycles SET vendor_id=?,vendor_period_id=?,cost_source='vendor_bill' WHERE id=?",&[p["vendor_id"].clone(),json!(new),created["id"].clone()]).await?;
                    exec(
                        db,
                        "UPDATE profiles SET vendor_id=? WHERE account_id=?",
                        &[p["vendor_id"].clone(), c["account_id"].clone()],
                    )
                    .await?;
                }
                next.push(json!({"previous_period_id":pid,"id":new}));
            }
            audit(db,actor,"settle_vendor",json!(pid),&json!({"cost_fen":p["cost_fen"],"end":p["settlement_end"],"members":members.iter().map(|c|c["id"].clone()).collect::<Vec<_>>()})).await?;
        }
        Ok(next)
    }
    pub async fn void_vendor(
        &self,
        db: &mut Db,
        actor: &Value,
        pid: i64,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        let p = one(db, "SELECT * FROM vendor_periods WHERE id=?", &[json!(pid)]).await?;
        if p["status"] != "settled" || p["version"] != data["version"] {
            return Err(Error::conflict("供应商结算已变化，请刷新"));
        }
        let members = Self::vendor_members(db, pid).await?;
        for c in &members {
            self.void_one(db, actor, int(c, "settlement_id")).await?;
        }
        exec(db,"UPDATE vendor_periods SET status='open',version=version+1,bill_digest=NULL,bill_difference_confirmed=0 WHERE id=?",&[json!(pid)]).await?;
        audit(db,actor,"void_vendor",json!(pid),&json!({"end":p["end"],"members":members.iter().map(|c|c["id"].clone()).collect::<Vec<_>>()})).await?;
        Ok(json!({"ok":true}))
    }
    pub async fn vendor_members(db: &mut Db, period: i64) -> Result<Vec<Value>> {
        all(db,"SELECT c.*,a.name AS current_name,a.deleted AS account_deleted FROM cycles c JOIN accounts a ON a.id=c.account_id WHERE c.vendor_period_id=? ORDER BY c.account_id", &[json!(period)]).await
    }
    pub async fn independent_cycle(c: &Value) -> Result<()> {
        if !c["vendor_period_id"].is_null() {
            return Err(Error::conflict("该账号属于供应商周期，请整组操作"));
        }
        Ok(())
    }
    pub async fn vendors(&self, actor: &Value) -> Result<Value> {
        let mut db = self.store.read().await?;
        let mut vendors=all(&mut db,"SELECT v.*,u.name AS creditor_name FROM vendors v LEFT JOIN users u ON u.id=v.creditor_id ORDER BY v.id",&[]).await?;
        for v in &mut vendors {
            let mut periods=all(&mut db,"SELECT p.*,u.name AS creditor_name FROM vendor_periods p LEFT JOIN users u ON u.id=p.creditor_id WHERE p.vendor_id=? ORDER BY p.id DESC",&[v["id"].clone()]).await?;
            for p in &mut periods {
                // Only public cycle metadata is returned here; raw bill rows stay in admin-only endpoints.
                p["members"] = json!(Self::vendor_members(&mut db, int(p, "id")).await?);
                if text(actor, "role") != "admin" {
                    p.as_object_mut().map(|o| o.remove("bill_digest"));
                }
            }
            v["periods"] = json!(periods);
        }
        Ok(json!({"vendors":vendors}))
    }
    pub async fn save_vendor(
        &self,
        db: &mut Db,
        actor: &Value,
        id: Option<i64>,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        one(
            db,
            "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
            &[data["creditor_id"].clone()],
        )
        .await?;
        let cost = cents(text(data, "cost"))?;
        let title = text(data, "name").trim();
        if title.is_empty() {
            return Err(Error::invalid("请填写供应商名称"));
        }
        let id = if let Some(id) = id {
            let old = one(db, "SELECT * FROM vendors WHERE id=?", &[json!(id)]).await?;
            if old["version"] != data["version"] || !old["archived_at"].is_null() {
                return Err(Error::conflict("供应商设置已变化，请刷新"));
            }
            exec(
                db,
                "UPDATE vendors SET name=?,cost_fen=?,creditor_id=?,version=version+1 WHERE id=?",
                &[
                    json!(title),
                    json!(cost),
                    data["creditor_id"].clone(),
                    json!(id),
                ],
            )
            .await?;
            id
        } else {
            exec(
                db,
                "INSERT INTO vendors(name,cost_fen,creditor_id,created_at) VALUES(?,?,?,?)",
                &[
                    json!(title),
                    json!(cost),
                    data["creditor_id"].clone(),
                    json!(time::now()),
                ],
            )
            .await?
            .last_insert_rowid()
        };
        audit(db, actor, "save_vendor", json!(id), data).await?;
        one(db, "SELECT * FROM vendors WHERE id=?", &[json!(id)]).await
    }
    pub async fn archive_vendor(
        &self,
        db: &mut Db,
        actor: &Value,
        id: i64,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        let v = one(db, "SELECT * FROM vendors WHERE id=?", &[json!(id)]).await?;
        if v["version"] != data["version"] {
            return Err(Error::conflict("供应商设置已变化，请刷新"));
        }
        if optional(
            db,
            "SELECT id FROM vendor_periods WHERE vendor_id=? AND status='open'",
            &[json!(id)],
        )
        .await?
        .is_some()
        {
            return Err(Error::conflict("请先结束供应商的开放周期"));
        }
        exec(
            db,
            "UPDATE vendors SET archived_at=?,version=version+1 WHERE id=?",
            &[json!(time::now()), json!(id)],
        )
        .await?;
        audit(db, actor, "archive_vendor", json!(id), &json!({})).await?;
        Ok(json!({"ok":true}))
    }
    pub async fn invalidate_vendor(db: &mut Db, period: i64) -> Result<()> {
        exec(db,"UPDATE vendor_periods SET bill_digest=NULL,bill_end=NULL,bill_difference_confirmed=0,version=version+1 WHERE id=?",&[json!(period)]).await?;
        exec(
            db,
            "DELETE FROM vendor_bill_previews WHERE vendor_period_id=?",
            &[json!(period)],
        )
        .await?;
        exec(db,"DELETE FROM previews WHERE EXISTS(SELECT 1 FROM json_each(previews.payload) j JOIN cycles c ON c.id=json_extract(j.value,'$.cycle.id') WHERE c.vendor_period_id=?)",&[json!(period)]).await?;
        Ok(())
    }
    pub async fn save_vendor_period(
        &self,
        db: &mut Db,
        actor: &Value,
        id: i64,
        data: &Value,
        editing: bool,
    ) -> Result<Value> {
        admin(actor)?;
        let old = if editing {
            Some(one(db, "SELECT * FROM vendor_periods WHERE id=?", &[json!(id)]).await?)
        } else {
            None
        };
        let vid = old.as_ref().map_or(id, |p| int(p, "vendor_id"));
        let vendor = one(db, "SELECT * FROM vendors WHERE id=?", &[json!(vid)]).await?;
        if !vendor["archived_at"].is_null() {
            return Err(Error::conflict("供应商已归档"));
        }
        let versioned = old.as_ref().unwrap_or(&vendor);
        if versioned["version"] != data["version"]
            || old.as_ref().is_some_and(|p| p["status"] != "open")
        {
            return Err(Error::conflict("供应商周期已变化，请刷新"));
        }
        one(
            db,
            "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
            &[data["creditor_id"].clone()],
        )
        .await?;
        let cost = cents(text(data, "cost"))?;
        let start = self.resolve_start(db, data).await?;
        if !editing
            && optional(
                db,
                "SELECT id FROM vendor_periods WHERE vendor_id=? AND status='open' AND end IS NULL",
                &[json!(vid)],
            )
            .await?
            .is_some()
        {
            return Err(Error::conflict("供应商已有开放周期，请修改整组配置"));
        }
        let previous = if editing {
            Self::vendor_members(db, id).await?
        } else {
            vec![]
        };
        let requested = array(&data["members"]);
        if requested.is_empty() || requested.len() > 100 {
            return Err(Error::invalid("请选择供应商周期成员"));
        }
        let ids = requested
            .iter()
            .map(|m| int(m, "cycle_id"))
            .collect::<BTreeSet<_>>();
        if ids.len() != requested.len() {
            return Err(Error::invalid("周期成员重复"));
        }
        let mut selected = Vec::new();
        for m in requested {
            let c=one(db,"SELECT c.*,a.deleted AS account_deleted,a.name AS current_name,p.kind FROM cycles c JOIN accounts a ON a.id=c.account_id AND a.root_id=a.id JOIN profiles p ON p.account_id=a.id WHERE c.id=?",&[m["cycle_id"].clone()]).await?;
            let existing = editing && c["vendor_period_id"] == id;
            if c["status"] != "open"
                || c["version"] != m["version"]
                || c["kind"] != "paid"
                || c["cost_kind"] != "exact"
            {
                return Err(Error::conflict("成员必须是最新的准确成本付费开放周期"));
            }
            if !existing && (flag(&c, "account_deleted") || !c["vendor_period_id"].is_null()) {
                return Err(Error::conflict("不能加入已删除账号或其他供应商周期"));
            }
            if !existing && c["start"] != start["start"] {
                return Err(Error::conflict("成员起点不一致，请先独立处理已有周期"));
            }
            if &c["end"] != old.as_ref().map_or(&Value::Null, |p| &p["end"]) {
                return Err(Error::conflict("成员结束时间不一致"));
            }
            if flag(&c, "start_locked")
                && (c["start"] != start["start"] || c["start_mode"] != start["start_mode"])
            {
                return Err(Error::conflict("升级付费起点已锁定，不能修改"));
            }
            self.validate_cycle(
                db,
                int(&c, "account_id"),
                text(&start, "start"),
                c["end"].as_str(),
                Some(int(&c, "id")),
                true,
            )
            .await?;
            selected.push(c);
        }
        for c in previous.iter().chain(selected.iter()) {
            Self::assert_not_deleting(db, int(c, "account_id")).await?;
            exec(db,"DELETE FROM previews WHERE EXISTS(SELECT 1 FROM json_each(previews.payload) j WHERE json_extract(j.value,'$.cycle.id')=?)",&[c["id"].clone()]).await?;
        }
        for c in &previous {
            if ids.contains(&int(c, "id")) {
                continue;
            }
            if flag(c, "account_deleted") {
                return Err(Error::conflict("已删除成员仍须保留本周期账务并整组结算"));
            }
            let d = array(&data["detached"])
                .iter()
                .find(|d| d["cycle_id"] == c["id"])
                .ok_or_else(|| Error::invalid("移出成员必须填写独立成本、垫付人和版本"))?;
            if d["version"] != c["version"] {
                return Err(Error::conflict("移出成员已变化，请刷新"));
            }
            one(
                db,
                "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
                &[d["creditor_id"].clone()],
            )
            .await?;
            exec(db,"UPDATE cycles SET vendor_id=NULL,vendor_period_id=NULL,cost_fen=?,creditor_id=?,cost_source='manual',bill_import_id=NULL,bill_digest=NULL,bill_end=NULL,bill_difference_confirmed=0,version=version+1 WHERE id=?",&[json!(cents(text(d,"cost"))?),d["creditor_id"].clone(),c["id"].clone()]).await?;
            exec(
                db,
                "UPDATE profiles SET vendor_id=NULL,version=version+1 WHERE account_id=?",
                &[c["account_id"].clone()],
            )
            .await?;
        }
        let same = old.as_ref().is_some_and(|p| {
            p["start"] == start["start"] && p["start_mode"] == start["start_mode"]
        });
        let confirmed = if same {
            old.as_ref().map_or(0, |p| int(p, "start_confirmed"))
        } else {
            int(&start, "start_confirmed")
        };
        // An unconfirmed incoming estimate must never become confirmed by grouping it.
        let confirmed = if selected
            .iter()
            .any(|c| c["start"] == start["start"] && !flag(c, "start_confirmed"))
        {
            0
        } else {
            confirmed
        };
        let pid = if editing {
            exec(db,"UPDATE vendor_periods SET cost_fen=?,creditor_id=?,start=?,start_mode=?,start_upper=?,start_confirmed=? WHERE id=?",&[json!(cost),data["creditor_id"].clone(),start["start"].clone(),start["start_mode"].clone(),start["start_upper"].clone(),json!(confirmed),json!(id)]).await?;
            Self::invalidate_vendor(db, id).await?;
            id
        } else {
            exec(db,"INSERT INTO vendor_periods(vendor_id,name,cost_fen,creditor_id,start,start_mode,start_upper,start_confirmed,created_at) VALUES(?,?,?,?,?,?,?,?,?)",&[json!(vid),vendor["name"].clone(),json!(cost),data["creditor_id"].clone(),start["start"].clone(),start["start_mode"].clone(),start["start_upper"].clone(),json!(confirmed),json!(time::now())]).await?.last_insert_rowid()
        };
        for c in selected {
            // No account cost is assigned before a bill splits the group cost.
            exec(db,"UPDATE cycles SET vendor_id=?,vendor_period_id=?,cost_fen=0,creditor_id=?,start=?,start_lower=?,start_mode=?,start_upper=?,start_confirmed=?,cost_source='vendor_bill',bill_import_id=NULL,bill_end=NULL,bill_digest=NULL,bill_difference_confirmed=0,version=version+1 WHERE id=?",&[json!(vid),json!(pid),data["creditor_id"].clone(),start["start"].clone(),start["start"].clone(),start["start_mode"].clone(),start["start_upper"].clone(),json!(confirmed),c["id"].clone()]).await?;
            exec(
                db,
                "UPDATE profiles SET vendor_id=?,version=version+1 WHERE account_id=?",
                &[json!(vid), c["account_id"].clone()],
            )
            .await?;
            exec(
                db,
                "DELETE FROM bill_previews WHERE cycle_id=?",
                &[c["id"].clone()],
            )
            .await?;
        }
        audit(
            db,
            actor,
            "save_vendor_period",
            json!(pid),
            &json!({"before":old,"configuration":data}),
        )
        .await?;
        one(db, "SELECT * FROM vendor_periods WHERE id=?", &[json!(pid)]).await
    }
    pub async fn confirm_vendor_start(
        &self,
        db: &mut Db,
        actor: &Value,
        id: i64,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        let p = one(db, "SELECT * FROM vendor_periods WHERE id=?", &[json!(id)]).await?;
        if p["status"] != "open" || p["version"] != data["version"] {
            return Err(Error::conflict("供应商周期已变化，请刷新"));
        }
        let start = time::instant(text(data, "start"))?;
        let members = Self::vendor_members(db, id).await?;
        if members.len() != array(&data["members"]).len() {
            return Err(Error::conflict("请核对完整成员集合"));
        }
        for c in &members {
            if !array(&data["members"])
                .iter()
                .any(|m| m["cycle_id"] == c["id"] && m["version"] == c["version"])
            {
                return Err(Error::conflict("成员已变化，请刷新"));
            }
            if flag(c, "start_locked") && c["start"] != start {
                return Err(Error::conflict("升级付费起点已锁定"));
            }
            self.validate_cycle(
                db,
                int(c, "account_id"),
                &start,
                p["end"].as_str(),
                Some(int(c, "id")),
                true,
            )
            .await?;
        }
        exec(db,"UPDATE vendor_periods SET start=?,start_mode='exact',start_upper=NULL,start_confirmed=1 WHERE id=?",&[json!(start),json!(id)]).await?;
        exec(db,"UPDATE cycles SET start=?,start_lower=?,start_mode='exact',start_upper=NULL,start_confirmed=1,start_confirmed_by=?,start_confirmed_at=?,version=version+1 WHERE vendor_period_id=?",&[json!(start),json!(start),actor["id"].clone(),json!(time::now()),json!(id)]).await?;
        Self::invalidate_vendor(db, id).await?;
        audit(
            db,
            actor,
            "confirm_vendor_start",
            json!(id),
            &json!({"before":p["start"],"after":start}),
        )
        .await?;
        Ok(json!({"id":id,"version":int(&p,"version")+1}))
    }
    pub async fn complete_vendor_selection(db: &mut Db, ids: &[Value]) -> Result<()> {
        let selected = ids
            .iter()
            .filter_map(Value::as_i64)
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        for id in ids {
            let c = one(
                db,
                "SELECT vendor_period_id FROM cycles WHERE id=?",
                std::slice::from_ref(id),
            )
            .await?;
            let pid = int(&c, "vendor_period_id");
            if pid == 0 || !seen.insert(pid) {
                continue;
            }
            for m in Self::vendor_members(db, pid).await? {
                if !selected.contains(&int(&m, "id")) {
                    return Err(Error::conflict(
                        "供应商周期必须包含全部成员，请刷新并整组结算",
                    ));
                }
            }
        }
        Ok(())
    }
}
