use super::*;
use crate::{
    json::{array, flag},
    money::cents,
    store::{all, exec, optional},
};
use std::collections::BTreeSet;

impl Service {
    // Current membership lives in profiles. Period membership is immutable after settlement.
    pub async fn current_vendor_members(db: &mut Db, vendor: i64) -> Result<Vec<Value>> {
        all(db, "SELECT a.id AS account_id,a.name,a.deleted AS account_deleted,p.version AS profile_version, EXISTS(SELECT 1 FROM account_deletions d JOIN cycles c ON c.settlement_id=d.settlement_id WHERE d.account_id=p.account_id AND c.vendor_id=p.vendor_id) AS renewal_excluded FROM profiles p JOIN accounts a ON a.id=p.account_id WHERE p.vendor_id=? ORDER BY a.id", &[json!(vendor)]).await
    }

    pub async fn assert_independent_account(db: &mut Db, account: i64) -> Result<()> {
        if optional(
            db,
            "SELECT vendor_id FROM profiles WHERE account_id=? AND vendor_id IS NOT NULL",
            &[json!(account)],
        )
        .await?
        .is_some()
        {
            return Err(Error::conflict("该账号属于账号组，请从组入口操作"));
        }
        Ok(())
    }

    async fn unconfigured_member(db: &mut Db, account: i64) -> Result<Value> {
        Self::assert_not_deleting(db, account).await?;
        let a = optional(
            db,
            "SELECT * FROM accounts WHERE id=? AND root_id=id AND deleted=0",
            &[json!(account)],
        )
        .await?
        .ok_or_else(|| Error::conflict("只能添加仍存在的待配置母账号"))?;
        // A missing profile is the persisted representation of an unconfigured account.
        if optional(
            db,
            "SELECT account_id FROM profiles WHERE account_id=?",
            &[json!(account)],
        )
        .await?
        .is_some()
            || optional(
                db,
                "SELECT id FROM cycles WHERE account_id=? AND status='open'",
                &[json!(account)],
            )
            .await?
            .is_some()
        {
            return Err(Error::conflict("只能添加待配置母账号，请刷新账号列表"));
        }
        Ok(a)
    }

    async fn insert_vendor_cycle(
        &self,
        db: &mut Db,
        period: &Value,
        a: &Value,
        previous: Value,
    ) -> Result<()> {
        self.validate_cycle(
            db,
            int(a, "account_id"),
            text(period, "start"),
            None,
            None,
            true,
        )
        .await?;
        exec(db,"INSERT INTO cycles(account_id,name,cost_fen,creditor_id,start,start_lower,start_upper,start_mode,start_confirmed,cost_kind,cost_source,vendor_id,vendor_period_id,renewed_from_cycle_id) VALUES(?,?,0,?,?,?,?,?,?,'exact','vendor_bill',?,?,?)",&[a["account_id"].clone(),a["name"].clone(),period["creditor_id"].clone(),period["start"].clone(),period["start"].clone(),period["start_upper"].clone(),period["start_mode"].clone(),period["start_confirmed"].clone(),period["vendor_id"].clone(),period["id"].clone(),previous]).await?;
        Ok(())
    }

    async fn join_vendor_members(
        &self,
        db: &mut Db,
        vid: i64,
        period: Option<&Value>,
        ids: &Value,
    ) -> Result<()> {
        let ids = array(ids);
        if ids.is_empty()
            || ids.len() > 100
            || ids
                .iter()
                .filter_map(Value::as_i64)
                .collect::<BTreeSet<_>>()
                .len()
                != ids.len()
        {
            return Err(Error::invalid("请选择不重复的待配置账号"));
        }
        if Self::current_vendor_members(db, vid).await?.len() + ids.len() > 100 {
            return Err(Error::invalid("账号组最多包含 100 个账号"));
        }
        let next_start = if period.is_none() {
            Some(one(db,"SELECT end FROM vendor_periods WHERE vendor_id=? AND status='settled' ORDER BY id DESC LIMIT 1",&[json!(vid)]).await?)
        } else {
            None
        };
        for id in ids {
            let a = Self::unconfigured_member(
                db,
                id.as_i64().ok_or_else(|| Error::invalid("账号无效"))?,
            )
            .await?;
            if let Some(previous) = &next_start {
                self.validate_cycle(db, int(&a, "id"), text(previous, "end"), None, None, true)
                    .await?;
            }
            if let Some(p) = period {
                self.insert_vendor_cycle(
                    db,
                    p,
                    &json!({"account_id":a["id"],"name":a["name"]}),
                    Value::Null,
                )
                .await?;
            }
            exec(
                db,
                "INSERT INTO profiles(account_id,name,kind,vendor_id) VALUES(?,?,'paid',?)",
                &[a["id"].clone(), a["name"].clone(), json!(vid)],
            )
            .await?;
        }
        Ok(())
    }

    pub async fn create_vendor_group_period(
        &self,
        db: &mut Db,
        vendor: &Value,
        data: &Value,
    ) -> Result<Value> {
        if optional(
            db,
            "SELECT id FROM vendor_periods WHERE vendor_id=?",
            &[vendor["id"].clone()],
        )
        .await?
        .is_some()
        {
            return Err(Error::conflict("账号组已有账期，请使用整组续费入口"));
        }
        let start = self.resolve_start(db, data).await?;
        let pid=exec(db,"INSERT INTO vendor_periods(vendor_id,name,cost_fen,creditor_id,start,start_mode,start_upper,start_confirmed,created_at) VALUES(?,?,?,?,?,?,?,?,?)",&[vendor["id"].clone(),vendor["name"].clone(),json!(cents(text(data,"cost"))?),data["creditor_id"].clone(),start["start"].clone(),start["start_mode"].clone(),start["start_upper"].clone(),start["start_confirmed"].clone(),json!(time::now())]).await?.last_insert_rowid();
        let p = one(db, "SELECT * FROM vendor_periods WHERE id=?", &[json!(pid)]).await?;
        self.join_vendor_members(db, int(vendor, "id"), Some(&p), &data["account_ids"])
            .await?;
        Ok(p)
    }

    pub async fn add_vendor_members(
        &self,
        db: &mut Db,
        actor: &Value,
        id: i64,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        let v = one(db, "SELECT * FROM vendors WHERE id=?", &[json!(id)]).await?;
        if v["version"] != data["version"] || !v["archived_at"].is_null() {
            return Err(Error::conflict("账号组已变化，请刷新"));
        }
        if optional(
            db,
            "SELECT id FROM vendor_periods WHERE vendor_id=?",
            &[json!(id)],
        )
        .await?
        .is_none()
        {
            return Err(Error::conflict("请先配置账号组首期，再追加账号"));
        }
        let p = optional(
            db,
            "SELECT * FROM vendor_periods WHERE vendor_id=? AND status='open' AND end IS NULL",
            &[json!(id)],
        )
        .await?;
        if p.as_ref().map_or(&Value::Null, |p| &p["version"]) != &data["period_version"] {
            return Err(Error::conflict("账号组账期已变化，请刷新"));
        }
        if p.is_none()
            && optional(
                db,
                "SELECT id FROM vendor_periods WHERE vendor_id=? AND status='open'",
                &[json!(id)],
            )
            .await?
            .is_some()
        {
            return Err(Error::conflict("请先处理已作废待重算账期，再追加账号"));
        }
        self.join_vendor_members(db, id, p.as_ref(), &data["account_ids"])
            .await?;
        if let Some(p) = &p {
            Self::invalidate_vendor(db, int(p, "id")).await?;
        }
        exec(
            db,
            "UPDATE vendors SET version=version+1 WHERE id=?",
            &[json!(id)],
        )
        .await?;
        audit(
            db,
            actor,
            "add_vendor_members",
            json!(id),
            &json!({"account_ids":data["account_ids"],"period_id":p.as_ref().map(|p| &p["id"])}),
        )
        .await?;
        Ok(json!({"id":id,"version":int(&v,"version")+1}))
    }

    async fn renewable_vendor_members(
        db: &mut Db,
        p: &Value,
        deletes: &Value,
    ) -> Result<Vec<Value>> {
        let old = Self::vendor_members(db, int(p, "id")).await?;
        let excluded = old
            .iter()
            .filter(|c| array(deletes).contains(&c["id"]))
            .map(|c| int(c, "account_id"))
            .collect::<BTreeSet<_>>();
        let mut eligible = Vec::new();
        for a in Self::current_vendor_members(db, int(p, "vendor_id")).await? {
            if flag(&a, "account_deleted") || excluded.contains(&int(&a, "account_id")) {
                continue;
            }
            // A cancelled/failed deletion still represents an explicit no-renewal decision.
            if optional(db,"SELECT id FROM account_deletions WHERE account_id=? AND settlement_id IN (SELECT settlement_id FROM cycles WHERE vendor_id=?)",&[a["account_id"].clone(),p["vendor_id"].clone()]).await?.is_some(){continue;}
            Self::assert_not_deleting(db, int(&a, "account_id")).await?;
            eligible.push(a);
        }
        if eligible.is_empty() {
            return Err(Error::conflict("没有可续费成员，请取消续费"));
        }
        Ok(eligible)
    }

    pub async fn check_vendor_renewal(
        db: &mut Db,
        p: &Value,
        data: &Value,
        deletes: &Value,
    ) -> Result<()> {
        let eligible = Self::renewable_vendor_members(db, p, deletes).await?;
        if let Some(ids) = data.get("cycle_ids") {
            let old = Self::vendor_members(db, int(p, "id")).await?;
            let wanted = old
                .iter()
                .filter(|c| eligible.iter().any(|a| a["account_id"] == c["account_id"]))
                .map(|c| int(c, "id"))
                .collect::<BTreeSet<_>>();
            let provided = array(ids)
                .iter()
                .filter_map(Value::as_i64)
                .collect::<BTreeSet<_>>();
            if provided != wanted {
                return Err(Error::conflict("账号组必须整组续费，不支持选择部分成员"));
            }
        }
        Ok(())
    }

    pub async fn renew_vendor_period(
        &self,
        db: &mut Db,
        actor: &Value,
        p: &Value,
        data: &Value,
        deletes: &Value,
    ) -> Result<Value> {
        Self::check_vendor_renewal(db, p, data, deletes).await?;
        if optional(
            db,
            "SELECT id FROM vendor_periods WHERE vendor_id=? AND status='open'",
            &[p["vendor_id"].clone()],
        )
        .await?
        .is_some()
        {
            return Err(Error::conflict("账号组已有开放账期，请先处理"));
        }
        one(
            db,
            "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
            &[data["creditor_id"].clone()],
        )
        .await?;
        let cost = cents(text(data, "cost"))?;
        let members = Self::renewable_vendor_members(db, p, deletes).await?;
        let vid = int(p, "vendor_id");
        let v = one(db, "SELECT * FROM vendors WHERE id=?", &[json!(vid)]).await?;
        if !v["archived_at"].is_null() {
            return Err(Error::conflict("账号组已归档"));
        }
        let pid=exec(db,"INSERT INTO vendor_periods(vendor_id,name,cost_fen,creditor_id,start,renewed_from_period_id,created_at) VALUES(?,?,?,?,?,?,?)",&[json!(vid),v["name"].clone(),json!(cost),data["creditor_id"].clone(),p["end"].clone(),p["id"].clone(),json!(time::now())]).await?.last_insert_rowid();
        let next = one(db, "SELECT * FROM vendor_periods WHERE id=?", &[json!(pid)]).await?;
        let previous = Self::vendor_members(db, int(p, "id")).await?;
        for a in &members {
            let prior = previous
                .iter()
                .find(|c| c["account_id"] == a["account_id"])
                .map_or(Value::Null, |c| c["id"].clone());
            self.insert_vendor_cycle(db, &next, a, prior).await?;
        }
        exec(
            db,
            "UPDATE vendors SET cost_fen=?,creditor_id=?,version=version+1 WHERE id=?",
            &[json!(cost), data["creditor_id"].clone(), json!(vid)],
        )
        .await?;
        audit(db,actor,"renew_vendor",json!(vid),&json!({"period_id":pid,"previous_period_id":p["id"],"account_ids":members.iter().map(|a| &a["account_id"]).collect::<Vec<_>>()})).await?;
        Ok(next)
    }

    pub async fn renew_vendor(
        &self,
        db: &mut Db,
        actor: &Value,
        id: i64,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        let v = one(db, "SELECT * FROM vendors WHERE id=?", &[json!(id)]).await?;
        if v["version"] != data["version"] {
            return Err(Error::conflict("账号组已变化，请刷新"));
        }
        let p = one(
            db,
            "SELECT * FROM vendor_periods WHERE vendor_id=? ORDER BY id DESC LIMIT 1",
            &[json!(id)],
        )
        .await?;
        if p["status"] != "settled" || p["version"] != data["period_version"] {
            return Err(Error::conflict("账号组账期已变化，请刷新"));
        }
        self.renew_vendor_period(db, actor, &p, data, &json!([]))
            .await
    }
}
