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
                return Err(Error::conflict("账号组结算依据已变化，请重新预览"));
            }
            p["settlement_end"] = snap["end"].clone();
            periods.insert(pid, p);
        }
        for r in array(&data["vendor_renewals"]) {
            let p = periods
                .get(&int(r, "period_id"))
                .ok_or_else(|| Error::invalid("续费账号组不在本次结算中"))?;
            if p["version"] != r["version"] {
                return Err(Error::conflict("账号组续费设置已变化"));
            }
            if optional(db,"SELECT id FROM vendor_periods WHERE vendor_id=? AND status='open' AND end IS NULL AND id!=?",&[p["vendor_id"].clone(),p["id"].clone()]).await?.is_some(){return Err(Error::conflict("该账号组已有续费周期，不能重复续费"));}
            Self::check_vendor_renewal(db, p, r, &data["delete_cycle_ids"]).await?;
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
            // Keep current profile membership after closing a period.
            exec(
                db,
                "UPDATE vendors SET version=version+1 WHERE id=?",
                &[p["vendor_id"].clone()],
            )
            .await?;
            if let Some(r) = array(&data["vendor_renewals"])
                .iter()
                .find(|r| r["period_id"] == p["id"])
            {
                let mut closed = p.clone();
                closed["end"] = p["settlement_end"].clone();
                let renewed = self
                    .renew_vendor_period(db, actor, &closed, r, &data["delete_cycle_ids"])
                    .await?;
                next.push(json!({"previous_period_id":pid,"id":renewed["id"]}));
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
            return Err(Error::conflict("账号组结算已变化，请刷新"));
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
            return Err(Error::conflict("该账号属于账号组周期，请整组操作"));
        }
        Ok(())
    }
    pub async fn vendors(&self, actor: &Value) -> Result<Value> {
        let state = self.inventory.refresh(false).await;
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
            v["current_members"] =
                json!(Self::current_vendor_members(&mut db, int(v, "id")).await?);
            v["current_period"] = periods
                .iter()
                .find(|p| p["status"] == "open" && p["end"].is_null())
                .cloned()
                .unwrap_or(Value::Null);
            v["waiting_renewal"] = json!(
                v["current_period"].is_null()
                    && periods.first().is_some_and(|p| p["status"] == "settled")
            );
            v["periods"] = json!(periods);
        }
        Ok(json!({"vendors":vendors,"account_revision":state["revision"]}))
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
            return Err(Error::invalid("请填写账号组名称"));
        }
        let creating = id.is_none();
        let id = if let Some(id) = id {
            let old = one(db, "SELECT * FROM vendors WHERE id=?", &[json!(id)]).await?;
            if old["version"] != data["version"] || !old["archived_at"].is_null() {
                return Err(Error::conflict("账号组设置已变化，请刷新"));
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
        let mut v = one(db, "SELECT * FROM vendors WHERE id=?", &[json!(id)]).await?;
        if creating && data.get("account_ids").is_some() {
            v["period"] = self.create_vendor_group_period(db, &v, data).await?;
        }
        Ok(v)
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
            return Err(Error::conflict("账号组设置已变化，请刷新"));
        }
        if optional(
            db,
            "SELECT id FROM vendor_periods WHERE vendor_id=? AND status='open'",
            &[json!(id)],
        )
        .await?
        .is_some()
        {
            return Err(Error::conflict("请先结束账号组的开放周期"));
        }
        exec(
            db,
            "UPDATE vendors SET archived_at=?,version=version+1 WHERE id=?",
            &[json!(time::now()), json!(id)],
        )
        .await?;
        let released = Self::current_vendor_members(db, id).await?;
        for a in &released {
            Self::assert_not_deleting(db, int(a, "account_id")).await?;
        }
        exec(db, "DELETE FROM profiles WHERE vendor_id=?", &[json!(id)]).await?;
        audit(
            db,
            actor,
            "archive_vendor",
            json!(id),
            &json!({"released_members":released}),
        )
        .await?;
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
        if !editing {
            let v = one(db, "SELECT * FROM vendors WHERE id=?", &[json!(id)]).await?;
            if v["version"] != data["version"] || !v["archived_at"].is_null() {
                return Err(Error::conflict("账号组已变化，请刷新"));
            }
            if data.get("account_ids").is_none() {
                return Err(Error::conflict(
                    "请直接选择待配置账号建组，已配置账号不能入组",
                ));
            }
            one(
                db,
                "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
                &[data["creditor_id"].clone()],
            )
            .await?;
            let p = self.create_vendor_group_period(db, &v, data).await?;
            exec(
                db,
                "UPDATE vendors SET version=version+1 WHERE id=?",
                &[json!(id)],
            )
            .await?;
            audit(db, actor, "save_vendor_period", p["id"].clone(), data).await?;
            return Ok(p);
        }
        let old = one(db, "SELECT * FROM vendor_periods WHERE id=?", &[json!(id)]).await?;
        let vid = int(&old, "vendor_id");
        let vendor = one(db, "SELECT * FROM vendors WHERE id=?", &[json!(vid)]).await?;
        if !vendor["archived_at"].is_null() {
            return Err(Error::conflict("账号组已归档"));
        }
        if old["status"] != "open" || old["version"] != data["version"] {
            return Err(Error::conflict("账号组周期已变化，请刷新"));
        }
        one(
            db,
            "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
            &[data["creditor_id"].clone()],
        )
        .await?;
        let cost = cents(text(data, "cost"))?;
        let start = self.resolve_start(db, data).await?;
        let members = Self::vendor_members(db, id).await?;
        let requested = array(&data["members"]);
        let ids = requested
            .iter()
            .map(|m| int(m, "cycle_id"))
            .collect::<BTreeSet<_>>();
        let previous_ids = members
            .iter()
            .map(|c| int(c, "id"))
            .collect::<BTreeSet<_>>();
        if ids.is_empty()
            || ids.len() != requested.len()
            || ids != previous_ids
            || !array(&data["detached"]).is_empty()
        {
            return Err(Error::conflict(
                "当前账期不能移出或替换成员；请使用添加账号入口追加待配置账号",
            ));
        }
        for c in &members {
            if c["status"] != "open"
                || !requested
                    .iter()
                    .any(|m| m["cycle_id"] == c["id"] && m["version"] == c["version"])
            {
                return Err(Error::conflict("成员已变化，请刷新"));
            }
            if flag(c, "start_locked")
                && (c["start"] != start["start"] || c["start_mode"] != start["start_mode"])
            {
                return Err(Error::conflict("升级付费起点已锁定，不能修改"));
            }
            self.validate_cycle(
                db,
                int(c, "account_id"),
                text(&start, "start"),
                old["end"].as_str(),
                Some(int(c, "id")),
                true,
            )
            .await?;
        }
        let same = old["start"] == start["start"] && old["start_mode"] == start["start_mode"];
        let confirmed = if same {
            int(&old, "start_confirmed")
        } else {
            int(&start, "start_confirmed")
        };
        exec(db,"UPDATE vendor_periods SET cost_fen=?,creditor_id=?,start=?,start_mode=?,start_upper=?,start_confirmed=? WHERE id=?",&[json!(cost),data["creditor_id"].clone(),start["start"].clone(),start["start_mode"].clone(),start["start_upper"].clone(),json!(confirmed),json!(id)]).await?;
        Self::invalidate_vendor(db, id).await?;
        for c in &members {
            // The group bill determines each member's amount; no independent cost is retained.
            exec(db,"UPDATE cycles SET cost_fen=0,creditor_id=?,start=?,start_lower=?,start_mode=?,start_upper=?,start_confirmed=?,cost_source='vendor_bill',bill_import_id=NULL,bill_end=NULL,bill_digest=NULL,bill_difference_confirmed=0,version=version+1 WHERE id=?",&[data["creditor_id"].clone(),start["start"].clone(),start["start"].clone(),start["start_mode"].clone(),start["start_upper"].clone(),json!(confirmed),c["id"].clone()]).await?;
            // Editing an old voided period must not reclaim a member released or configured elsewhere.
            exec(
                db,
                "UPDATE profiles SET version=version+1 WHERE vendor_id=? AND account_id=?",
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
        exec(
            db,
            "UPDATE vendors SET cost_fen=?,creditor_id=?,version=version+1 WHERE id=?",
            &[json!(cost), data["creditor_id"].clone(), json!(vid)],
        )
        .await?;
        audit(
            db,
            actor,
            "save_vendor_period",
            json!(id),
            &json!({"before":old,"configuration":data}),
        )
        .await?;
        one(db, "SELECT * FROM vendor_periods WHERE id=?", &[json!(id)]).await
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
            return Err(Error::conflict("账号组周期已变化，请刷新"));
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
                        "账号组周期必须包含全部成员，请刷新并整组结算",
                    ));
                }
            }
        }
        Ok(())
    }
}
