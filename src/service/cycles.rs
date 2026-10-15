use super::*;
use crate::{
    json::{array, flag, merge},
    money::cents,
    store::{exec, optional},
};
use std::collections::HashSet;

pub fn estimate(mode: &str) -> bool {
    matches!(mode, "date_estimate" | "minute_estimate")
}
pub fn start_meta(c: &Value) -> Value {
    let confirmed = c
        .get("start_confirmed")
        .map(|_| flag(c, "start_confirmed"))
        .unwrap_or(true);
    json!({"start_mode":if text(c,"start_mode").is_empty(){"exact"}else{text(c,"start_mode")},"start_lower":c.get("start_lower").filter(|v|!v.is_null()).unwrap_or(&c["start"]),"start_upper":c["start_upper"],"start_confirmed":confirmed,"start_confirmed_by":c["start_confirmed_by"],"start_confirmed_at":c["start_confirmed_at"],"start_source_usage_id":c["start_source_usage_id"],"start_warning":if confirmed{Value::Null}else{json!("周期起点未确认")}})
}
impl Service {
    pub async fn resolve_start(&self, db: &mut Db, data: &Value) -> Result<Value> {
        let mode = if text(data, "start_mode").is_empty() {
            "exact"
        } else {
            text(data, "start_mode")
        };
        if mode == "earliest_usage" {
            let u = store::one(
                db,
                "SELECT id,created_at FROM usage WHERE root_id=? ORDER BY created_at,id LIMIT 1",
                &[data["account_id"].clone()],
                "该账号尚无已采集使用记录，无法从最早使用开始",
            )
            .await
            .map_err(|_| Error::invalid("该账号尚无已采集使用记录，无法从最早使用开始"))?;
            return Ok(
                json!({"start":u["created_at"],"start_lower":u["created_at"],"start_upper":null,"start_mode":mode,"start_confirmed":1,"start_confirmed_by":data["start_confirmed_by"],"start_confirmed_at":data["start_confirmed_at"],"start_source_usage_id":u["id"]}),
            );
        }
        if estimate(mode) {
            let (lo, hi) = if mode == "minute_estimate" {
                time::minute(text(data, "start"))?
            } else {
                time::date_bounds(text(data, "start"))?
            };
            return Ok(
                json!({"start":lo,"start_lower":lo,"start_upper":hi,"start_mode":mode,"start_confirmed":0,"start_confirmed_by":null,"start_confirmed_at":null,"start_source_usage_id":null}),
            );
        }
        if mode != "exact" {
            return Err(Error::invalid("周期起点方式无效"));
        }
        if text(data, "start").is_empty() {
            return Err(Error::invalid("精确时间起点不能为空"));
        }
        let start = time::instant(text(data, "start"))?;
        Ok(
            json!({"start":start,"start_lower":start,"start_upper":null,"start_mode":"exact","start_confirmed":1,"start_confirmed_by":data["start_confirmed_by"],"start_confirmed_at":data["start_confirmed_at"],"start_source_usage_id":null}),
        )
    }
    pub async fn coverage_error(db: &mut Db, cycle: &Value, end: &str) -> Result<Option<String>> {
        let coverage = store::meta(db, "coverage_from").await?;
        if coverage.is_null() || text(cycle, "start") < coverage.as_str().unwrap_or("") {
            return Ok(Some("周期起点早于已采集记录覆盖范围".into()));
        }
        let gaps = store::meta(db, "coverage_gaps").await?;
        for gap in array(&gaps) {
            if (gap["root_id"].is_null() || gap["root_id"] == cycle["account_id"])
                && text(cycle, "start") < text(gap, "end")
                && end > text(gap, "start")
            {
                return Ok(Some("该周期存在未采集的历史记录，无法确认完整消费".into()));
            }
        }
        Ok(None)
    }
    pub async fn validate_cycle(
        &self,
        db: &mut Db,
        account: i64,
        start: &str,
        end: Option<&str>,
        exclude: Option<i64>,
        allow_estimate: bool,
    ) -> Result<()> {
        Self::assert_not_deleting(db, account).await?;
        let a = store::one(
            db,
            "SELECT * FROM accounts WHERE id=? AND root_id=id",
            &[json!(account)],
            "请选择母账号",
        )
        .await?;
        if exclude.is_none() && flag(&a, "deleted") {
            return Err(Error::conflict("账号已删除，不能创建新周期"));
        }
        let coverage = store::meta(db, "coverage_from").await?;
        if !allow_estimate
            && (coverage.is_null()
                || start < coverage.as_str().unwrap_or("")
                || start < text(&a, "created_at"))
        {
            return Err(Error::invalid(
                "开始时间超出现有记录覆盖范围，请核对可用起点",
            ));
        }
        let now = time::now();
        let limit = end.unwrap_or(&now);
        if let Some(gap) =
            Self::coverage_error(db, &json!({"account_id":account,"start":start}), limit).await?
            && !allow_estimate
            && exclude.is_none()
        {
            return Err(Error::invalid(gap));
        }
        if start >= limit {
            return Err(Error::invalid("开始时间必须早于结束时间"));
        }
        if optional(db,"SELECT 1 AS found FROM cycles WHERE account_id=? AND id!=? AND start<? AND coalesce(end,'9999')>?",&[json!(account),json!(exclude.unwrap_or(-1)),json!(end.unwrap_or("9999")),json!(start)]).await?.is_some(){return Err(Error::conflict("该账号已有重叠周期"));}
        Ok(())
    }
    pub async fn create_cycle(&self, db: &mut Db, actor: &Value, data: &Value) -> Result<Value> {
        admin(actor)?;
        let account = int(data, "account_id");
        if optional(
            db,
            "SELECT kind FROM profiles WHERE account_id=?",
            &[json!(account)],
        )
        .await?
        .is_some_and(|p| p["kind"] == "free")
        {
            return Err(Error::conflict("免费账号请使用升级付费入口"));
        }
        store::one(
            db,
            "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
            &[data["creditor_id"].clone()],
            "垫付人不可用",
        )
        .await?;
        let start = self.resolve_start(db, data).await?;
        self.validate_cycle(
            db,
            account,
            text(&start, "start"),
            None,
            None,
            estimate(text(&start, "start_mode")),
        )
        .await?;
        let cost = cents(text(data, "cost"))?;
        let title = text(data, "name").trim();
        if title.is_empty() {
            return Err(Error::invalid("请填写账号公开名称"));
        }
        exec(db,"INSERT INTO profiles(account_id,name,kind) VALUES(?,?,'paid') ON CONFLICT(account_id) DO UPDATE SET name=excluded.name,kind='paid',version=profiles.version+1",&[json!(account),json!(title)]).await?;
        let cid=exec(db,"INSERT INTO cycles(account_id,name,cost_fen,creditor_id,start,start_mode,start_lower,start_upper,start_confirmed,start_confirmed_by,start_confirmed_at,start_source_usage_id,renewed_from_cycle_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",&[json!(account),json!(title),json!(cost),data["creditor_id"].clone(),start["start"].clone(),start["start_mode"].clone(),start["start_lower"].clone(),start["start_upper"].clone(),start["start_confirmed"].clone(),start["start_confirmed_by"].clone(),start["start_confirmed_at"].clone(),start["start_source_usage_id"].clone(),data["renewed_from_cycle_id"].clone()]).await?.last_insert_rowid();
        let kind = if text(data, "cost_kind").is_empty() {
            "exact"
        } else {
            text(data, "cost_kind")
        };
        exec(
            db,
            "UPDATE cycles SET cost_kind=? WHERE id=?",
            &[json!(kind), json!(cid)],
        )
        .await?;
        audit(db,actor,"create_cycle",json!(cid),&merge(start,json!({"cost_fen":cost,"cost_kind":kind,"creditor_id":data["creditor_id"],"renewed_from_cycle_id":data["renewed_from_cycle_id"]}))).await?;
        Ok(json!({"id":cid}))
    }
    pub async fn upgrade_paid(
        &self,
        db: &mut Db,
        actor: &Value,
        account: i64,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        store::one(
            db,
            "SELECT id FROM accounts WHERE id=? AND root_id=id",
            &[json!(account)],
            "请选择母账号",
        )
        .await?;
        let profile = store::one(
            db,
            "SELECT * FROM profiles WHERE account_id=?",
            &[json!(account)],
            "账号尚未标记为免费",
        )
        .await?;
        if profile["kind"] != "free" {
            return Err(Error::conflict("只有免费账号可以升级付费"));
        }
        if profile["version"] != data["version"] {
            return Err(Error::conflict("账号配置已更新，请刷新后再升级"));
        }
        if optional(
            db,
            "SELECT 1 AS found FROM cycles WHERE account_id=? AND status='open'",
            &[json!(account)],
        )
        .await?
        .is_some()
        {
            return Err(Error::conflict("请先结束当前付费周期"));
        }
        store::one(
            db,
            "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
            &[data["creditor_id"].clone()],
            "垫付人不可用",
        )
        .await?;
        let title = text(data, "name").trim();
        if title.is_empty() {
            return Err(Error::invalid("请填写账号公开名称"));
        }
        let cost = cents(text(data, "cost"))?;
        let since = time::now();
        self.validate_cycle(db, account, &since, None, None, true)
            .await?;
        let version = int(&profile, "version") + 1;
        exec(
            db,
            "UPDATE profiles SET name=?,kind='paid',version=? WHERE account_id=? AND version=?",
            &[
                json!(title),
                json!(version),
                json!(account),
                profile["version"].clone(),
            ],
        )
        .await?;
        let kind = if text(data, "cost_kind").is_empty() {
            "exact"
        } else {
            text(data, "cost_kind")
        };
        let cid=exec(db,"INSERT INTO cycles(account_id,name,cost_fen,creditor_id,start,start_mode,start_lower,start_upper,start_confirmed,start_confirmed_by,start_confirmed_at,start_source_usage_id,renewed_from_cycle_id,start_locked,cost_kind) VALUES(?,?,?,?,?,'exact',?,NULL,1,?,?,NULL,NULL,1,?)",&[json!(account),json!(title),json!(cost),data["creditor_id"].clone(),json!(since),json!(since),actor["id"].clone(),json!(since),json!(kind)]).await?.last_insert_rowid();
        audit(db,actor,"upgrade_paid",json!(account),&json!({"cycle_id":cid,"paid_since":since,"cost_fen":cost,"cost_kind":kind,"creditor_id":data["creditor_id"],"profile_version":version})).await?;
        Ok(json!({"account_id":account,"cycle_id":cid,"paid_since":since,"version":version}))
    }
    pub async fn edit_cycle(
        &self,
        db: &mut Db,
        actor: &Value,
        cid: i64,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        let c = one(db, "SELECT * FROM cycles WHERE id=?", &[json!(cid)]).await?;
        if c["status"] != "open" {
            return Err(Error::conflict("已出单成本已锁定"));
        }
        if c["version"] != data["version"] {
            return Err(Error::conflict("周期已更新，请刷新后再修改"));
        }
        let requested_mode = if text(data, "start_mode").is_empty() {
            "exact"
        } else {
            text(data, "start_mode")
        };
        if flag(&c, "start_locked")
            && (requested_mode != text(&c, "start_mode")
                || time::instant(text(data, "start")).ok().as_deref() != Some(text(&c, "start")))
        {
            return Err(Error::conflict("升级付费起点已锁定，不能修改"));
        }
        if data["creditor_id"] != c["creditor_id"] {
            store::one(
                db,
                "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
                &[data["creditor_id"].clone()],
                "垫付人不可用",
            )
            .await?;
        }
        let mut start = self
            .resolve_start(
                db,
                &merge(data.clone(), json!({"account_id":c["account_id"]})),
            )
            .await?;
        let same = requested_mode == text(&c, "start_mode")
            && (c["start_mode"] == "earliest_usage" || start["start"] == c["start"]);
        if same {
            for k in [
                "start",
                "start_mode",
                "start_lower",
                "start_upper",
                "start_confirmed",
                "start_confirmed_by",
                "start_confirmed_at",
                "start_source_usage_id",
            ] {
                start[k] = c[k].clone();
            }
        }
        self.validate_cycle(
            db,
            int(&c, "account_id"),
            text(&start, "start"),
            c["end"].as_str(),
            Some(cid),
            same || estimate(text(&start, "start_mode")),
        )
        .await?;
        let cost = cents(text(data, "cost"))?;
        let title = text(data, "name").trim();
        if title.is_empty() {
            return Err(Error::invalid("请填写账号公开名称"));
        }
        exec(db,"UPDATE cycles SET name=?,cost_fen=?,creditor_id=?,start=?,start_mode=?,start_lower=?,start_upper=?,start_confirmed=?,start_confirmed_by=?,start_confirmed_at=?,start_source_usage_id=?,start_anchor=?,cost_kind=?,version=version+1 WHERE id=?",&[json!(title),json!(cost),data["creditor_id"].clone(),start["start"].clone(),start["start_mode"].clone(),start["start_lower"].clone(),start["start_upper"].clone(),start["start_confirmed"].clone(),start["start_confirmed_by"].clone(),start["start_confirmed_at"].clone(),start["start_source_usage_id"].clone(),if same{c["start_anchor"].clone()}else{Value::Null},data.get("cost_kind").filter(|v|!v.is_null()).unwrap_or(&c["cost_kind"]).clone(),json!(cid)]).await?;
        if cost != int(&c, "cost_fen") {
            exec(db,"UPDATE cycles SET cost_source='manual',bill_import_id=NULL,bill_end=NULL,bill_digest=NULL,bill_difference_confirmed=0 WHERE id=?",&[json!(cid)]).await?;
        }
        exec(
            db,
            "UPDATE profiles SET name=?,version=version+1 WHERE account_id=?",
            &[json!(title), c["account_id"].clone()],
        )
        .await?;
        audit(
            db,
            actor,
            "edit_cycle",
            json!(cid),
            &json!({"before":c,"after":merge(merge(data.clone(),json!({"cost_fen":cost})),start)}),
        )
        .await?;
        Ok(json!({"id":cid,"version":int(&c,"version")+1}))
    }
    pub async fn confirm_start(
        &self,
        db: &mut Db,
        actor: &Value,
        cid: i64,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        let c = one(db, "SELECT * FROM cycles WHERE id=?", &[json!(cid)]).await?;
        if c["status"] != "open" {
            return Err(Error::conflict("已结算周期不能确认起点"));
        }
        if c["version"] != data["version"] {
            return Err(Error::conflict("周期已更新，请重新预览"));
        }
        if !estimate(text(&c, "start_mode")) || flag(&c, "start_confirmed") {
            return Err(Error::invalid("该周期无需确认起点"));
        }
        let method = if text(data, "method").is_empty() {
            "legacy"
        } else {
            text(data, "method")
        };
        let anchor = time::instant(text(data, "start"))?;
        let mut start = anchor.clone();
        let mut source_id = Value::Null;
        if method == "first_usage_after" {
            let first=optional(db,"SELECT id,created_at FROM usage WHERE root_id=? AND created_at>=? AND created_at<? ORDER BY created_at,id LIMIT 1",&[c["account_id"].clone(),json!(anchor),c["end"].as_str().map(|s|json!(s)).unwrap_or_else(||json!(time::now()))]).await?.ok_or_else(||Error::invalid("充值时间后尚无已采集请求，请保留待确认状态"))?;
            if data["source_usage_id"] != first["id"] {
                return Err(Error::conflict("首请求已变化，请重新查询后确认"));
            }
            start = text(&first, "created_at").to_owned();
            source_id = first["id"].clone();
        }
        let lower = c["start_lower"].as_str().unwrap_or(text(&c, "start"));
        if method == "legacy" && c["start_mode"] == "minute_estimate" && start != lower {
            return Err(Error::invalid("请确认所选分钟起点，修改时间后需重新预览"));
        }
        if method == "legacy"
            && c["start_upper"]
                .as_str()
                .is_some_and(|upper| start.as_str() < lower || start.as_str() >= upper)
        {
            return Err(Error::invalid("确认时间必须位于估算日期范围内"));
        }
        self.validate_cycle(
            db,
            int(&c, "account_id"),
            &start,
            c["end"].as_str(),
            Some(cid),
            method != "legacy",
        )
        .await?;
        if method == "legacy"
            && Self::coverage_error(
                db,
                &json!({"account_id":c["account_id"],"start":start}),
                c["end"].as_str().unwrap_or(&time::now()),
            )
            .await?
            .is_some()
        {
            return Err(Error::invalid(
                "该周期存在未采集的历史记录，无法确认完整消费",
            ));
        }
        let confirmed = time::now();
        exec(db,"UPDATE cycles SET start=?,start_lower=?,start_upper=NULL,start_mode='exact',start_confirmed=1,start_confirmed_by=?,start_confirmed_at=?,start_anchor=?,start_source_usage_id=?,version=version+1 WHERE id=? AND version=?",&[json!(start),json!(start),actor["id"].clone(),json!(confirmed),if method=="first_usage_after"{json!(anchor)}else{Value::Null},source_id.clone(),json!(cid),c["version"].clone()]).await?;
        audit(db,actor,"confirm_cycle_start",json!(cid),&json!({"before":{"start":c["start"],"start_mode":c["start_mode"],"start_lower":lower,"start_upper":c["start_upper"]},"start":start,"confirmed_at":confirmed,"method":method,"anchor":anchor,"source_usage_id":source_id})).await?;
        Ok(json!({"id":cid,"version":int(&c,"version")+1,"start":start,"start_confirmed":true}))
    }
    pub async fn free(
        &self,
        db: &mut Db,
        actor: &Value,
        account: i64,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        let title = text(data, "name").trim();
        if title.is_empty() {
            return Err(Error::invalid("请填写账号公开名称"));
        }
        one(
            db,
            "SELECT id FROM accounts WHERE id=? AND root_id=id",
            &[json!(account)],
        )
        .await?;
        if optional(
            db,
            "SELECT 1 AS found FROM cycles WHERE account_id=? AND status='open'",
            &[json!(account)],
        )
        .await?
        .is_some()
        {
            return Err(Error::conflict("请先结束当前付费周期"));
        }
        let prior = optional(
            db,
            "SELECT version FROM profiles WHERE account_id=?",
            &[json!(account)],
        )
        .await?;
        if prior
            .as_ref()
            .map(|p| &p["version"])
            .unwrap_or(&Value::Null)
            != &data["version"]
        {
            return Err(Error::conflict("账号配置已更新"));
        }
        exec(db,"INSERT INTO profiles(account_id,name,kind) VALUES(?,?,'free') ON CONFLICT(account_id) DO UPDATE SET name=excluded.name,kind='free',version=profiles.version+1",&[json!(account),json!(title)]).await?;
        audit(db, actor, "mark_free", json!(account), data).await?;
        Ok(json!({"id":account}))
    }
    pub async fn start_candidate(&self, actor: &Value, cid: i64, after: &str) -> Result<Value> {
        admin(actor)?;
        let after = time::instant(after)?;
        let mut db = self.store.read().await?;
        Self::fresh(&mut db).await?;
        let c = one(&mut db, "SELECT * FROM cycles WHERE id=?", &[json!(cid)]).await?;
        let u=optional(&mut db,"SELECT id,created_at FROM usage WHERE root_id=? AND created_at>=? AND created_at<? ORDER BY created_at,id LIMIT 1",&[c["account_id"].clone(),json!(after),json!(c["end"].as_str().unwrap_or(&time::now()))]).await?;
        Ok(json!({"version":c["version"],"anchor":after,"usage":u}))
    }
    pub async fn save_expense(
        &self,
        db: &mut Db,
        actor: &Value,
        data: &Value,
        eid: Option<i64>,
    ) -> Result<Value> {
        admin(actor)?;
        let old = if let Some(id) = eid {
            Some(one(db, "SELECT * FROM expenses WHERE id=?", &[json!(id)]).await?)
        } else {
            None
        };
        if old
            .as_ref()
            .is_some_and(|o| o["status"] != "open" || o["version"] != data["version"])
        {
            return Err(Error::conflict("费用已出单或已修改，请刷新"));
        }
        let title = text(data, "name").trim();
        if title.is_empty() {
            return Err(Error::invalid("请填写费用名称"));
        }
        let cost = cents(text(data, "cost"))?;
        if cost <= 0 {
            return Err(Error::invalid("固定费用须大于零"));
        }
        let members = array(&data["members"]);
        let ids: HashSet<_> = members.iter().map(|m| int(m, "user_id")).collect();
        if ids.is_empty() || ids.len() != members.len() {
            return Err(Error::invalid("请选择不同的参与用户"));
        }
        let existing: HashSet<i64> = if let Some(old) = &old {
            let members: Value = serde_json::from_str(text(old, "members"))?;
            array(&members).iter().map(|m| int(m, "user_id")).collect()
        } else {
            HashSet::new()
        };
        for uid in ids.difference(&existing) {
            store::one(
                db,
                "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
                &[json!(uid)],
                "参与用户或垫付人不可用",
            )
            .await?;
        }
        if old
            .as_ref()
            .is_none_or(|o| o["creditor_id"] != data["creditor_id"])
        {
            store::one(
                db,
                "SELECT id FROM users WHERE id=? AND deleted=0 AND status='active'",
                &[data["creditor_id"].clone()],
                "垫付人不可用",
            )
            .await?;
        }
        let mode = text(data, "allocation_mode");
        let mut stored = Vec::new();
        let mut total = 0i64;
        for m in members {
            let weight = if mode == "equal" {
                1
            } else {
                cents(if text(m, "percent").is_empty() {
                    "0"
                } else {
                    text(m, "percent")
                })?
            };
            total += weight;
            stored.push(json!({"user_id":m["user_id"],"weight":weight}));
        }
        if mode == "custom" && total != 10000 {
            return Err(Error::invalid("参与比例合计必须为 100.00%"));
        }
        let date = text(data, "incurred_on");
        let id = if let Some(id) = eid {
            exec(db,"UPDATE expenses SET name=?,cost_fen=?,incurred_on=?,creditor_id=?,allocation_mode=?,members=?,version=version+1 WHERE id=?",&[json!(title),json!(cost),json!(date),data["creditor_id"].clone(),json!(mode),json!(dumps(&json!(stored))),json!(id)]).await?;
            id
        } else {
            exec(db,"INSERT INTO expenses(name,cost_fen,incurred_on,creditor_id,allocation_mode,members,created_at) VALUES(?,?,?,?,?,?,?)",&[json!(title),json!(cost),json!(date),data["creditor_id"].clone(),json!(mode),json!(dumps(&json!(stored))),json!(time::now())]).await?.last_insert_rowid()
        };
        audit(
            db,
            actor,
            if eid.is_some() {
                "edit_expense"
            } else {
                "create_expense"
            },
            json!(id),
            &json!({"before":old,"after":data}),
        )
        .await?;
        Ok(json!({"id":id,"version":old.as_ref().map(|o|int(o,"version")+1).unwrap_or(1)}))
    }
}
