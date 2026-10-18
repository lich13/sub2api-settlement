use super::*;
use crate::{
    billing::{self, BillRow, RequestDetail, UsageRow},
    json::{array, flag, merge, without},
    money::{Decimal, allocate},
    store::exec,
};

/// R5 has two independent largest-remainder boundaries: account ID, then user ID.
/// With M>0, M_a + U*M_a/M has exactly the same ratios as M_a. Keeping M_a
/// avoids rounding the rational intermediate weights or changing independent bills.
pub fn vendor_allocations(
    cost: i64,
    creditor: i64,
    members: &[Value],
    usage: &[UsageRow],
    matched: &Value,
) -> Result<Value> {
    vendor_allocations_with_manual(cost, creditor, members, usage, matched, None)
}
fn vendor_allocations_with_manual(
    cost: i64,
    creditor: i64,
    members: &[Value],
    usage: &[UsageRow],
    matched: &Value,
    manual: Option<&Value>,
) -> Result<Value> {
    let roots = usage
        .iter()
        .map(|u| (u.id, u.root_id))
        .collect::<BTreeMap<_, _>>();
    let mut local = BTreeMap::<i64, BTreeMap<i64, Decimal>>::new();
    for u in usage {
        local
            .entry(u.root_id)
            .or_default()
            .entry(u.user_id)
            .or_insert_with(Decimal::zero)
            .add(&Decimal::parse(&u.amount)?);
    }
    let mut upstream = BTreeMap::<i64, BTreeMap<i64, Decimal>>::new();
    for r in array(matched) {
        let root = roots
            .get(&int(r, "usage_id"))
            .ok_or_else(Error::unavailable)?;
        upstream
            .entry(*root)
            .or_default()
            .entry(int(r, "user_id"))
            .or_insert_with(Decimal::zero)
            .add(&Decimal::parse(text(r, "amount"))?);
    }
    if let Some(manual) = manual {
        for r in array(&manual["local_unmatched"])
            .iter()
            .filter(|r| flag(r, "included"))
        {
            upstream
                .entry(int(r, "account_id"))
                .or_default()
                .entry(int(r, "user_id"))
                .or_insert_with(Decimal::zero)
                .add(&Decimal::parse(text(r, "amount"))?);
        }
    }
    let sum = |weights: &BTreeMap<i64, Decimal>| {
        let mut n = Decimal::zero();
        for v in weights.values() {
            n.add(v);
        }
        n
    };
    let upstream_total = sum(&upstream.iter().map(|(id, w)| (*id, sum(w))).collect());
    let from_local = manual.is_none() && !upstream_total.positive();
    let source = if from_local { &local } else { &upstream };
    let weights = members
        .iter()
        .map(|c| {
            let id = int(c, "account_id");
            (id, source.get(&id).map_or_else(Decimal::zero, sum))
        })
        .collect::<BTreeMap<_, _>>();
    let positive = weights.values().any(Decimal::positive);
    let account_shares = if positive {
        allocate(cost, &weights, -1, 60)?
    } else {
        json!([])
    };
    let mut accounts = Vec::new();
    for c in members {
        let aid = int(c, "account_id");
        let amount = array(&account_shares)
            .iter()
            .find(|a| int(a, "user_id") == aid)
            .map_or(0, |a| int(a, "amount_fen"));
        let own = if manual.is_some() {
            source.get(&aid).cloned().unwrap_or_default()
        } else {
            source
                .get(&aid)
                .or(local.get(&aid))
                .cloned()
                .unwrap_or_default()
        };
        let allocations = if positive {
            allocate(amount, &own, creditor, 60)?
        } else {
            json!([])
        };
        accounts.push(json!({"account_id":aid,"cycle_id":c["id"],"name":c["current_name"].as_str().unwrap_or(text(c,"name")),"amount_fen":if positive{json!(amount)}else{Value::Null},"weight":weights[&aid].to_string(),"total_consumption":local.get(&aid).map_or_else(Decimal::zero,sum).to_string(),"allocations":allocations}));
    }
    Ok(
        json!({"accounts":accounts,"basis":if manual.is_some(){"manual_supplement"}else if from_local{"local_consumption"}else{"matched_upstream"},"can_confirm":positive}),
    )
}

impl Service {
    pub async fn vendor_calculation(
        &self,
        db: &mut Db,
        pid: i64,
        end: &str,
        rows: Option<Vec<BillRow>>,
        include_records: bool,
    ) -> Result<Value> {
        self.vendor_calculation_basis(db, pid, end, rows, include_records, None)
            .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn vendor_calculation_basis(
        &self,
        db: &mut Db,
        pid: i64,
        end: &str,
        rows: Option<Vec<BillRow>>,
        include_records: bool,
        manual_override: Option<Value>,
    ) -> Result<Value> {
        let period = one(db, "SELECT * FROM vendor_periods WHERE id=?", &[json!(pid)]).await?;
        let pending_manual = Self::has_manual_draft(db, None, Some(pid)).await?;
        let manual = if rows.is_none() {
            Self::manual_config(db, &period).await?
        } else {
            manual_override
        };
        let members = Self::vendor_members(db, pid).await?;
        let mut usage = Vec::new();
        let mut details = BTreeMap::<i64, RequestDetail>::new();
        let mut versions = Vec::new();
        let mut gaps = Vec::new();
        for c in &members {
            usage.extend(Self::usage(db, c, end).await?);
            details.extend(Self::details(db, c, end).await?);
            versions.push(json!({"cycle_id":c["id"],"version":c["version"],"account_id":c["account_id"],"usage_version":store::revision(db,int(c,"account_id")).await?}));
            if let Some(gap) = Self::coverage_error(db, c, end).await? {
                gaps.push(json!({"account_id":c["account_id"],"warning":gap}));
            }
        }
        usage.sort_by_key(|u| u.id);
        let names = Self::names(db).await?;
        let stored = if rows.is_none() && !period["bill_import_id"].is_null() {
            Some(
                one(
                    db,
                    "SELECT * FROM vendor_bill_imports WHERE id=? AND vendor_period_id=?",
                    &[period["bill_import_id"].clone(), json!(pid)],
                )
                .await?,
            )
        } else {
            None
        };
        let rows = match rows {
            Some(rows) => Some(rows),
            None => stored
                .as_ref()
                .map(|b| serde_json::from_str::<Vec<BillRow>>(text(b, "rows")))
                .transpose()?,
        };
        let end = end.to_owned();
        let permit = self
            .cpu
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::unavailable())?;
        tokio::task::spawn_blocking(move||{
            let _permit=permit;
            let resolved = if let Some(config) = &manual {Some(crate::manual_billing::reconcile(rows.as_deref().unwrap_or(&[]),&usage,&details,text(&period,"start"),&end,"proportional",config,&members.iter().map(|c|int(c,"account_id")).collect::<Vec<_>>())?)}else{rows.as_ref().map(|r|billing::reconcile(r,&usage,&details,text(&period,"start"),&end,"proportional")).transpose()?};
            let mut split=vendor_allocations_with_manual(int(&period,"cost_fen"),int(&period,"creditor_id"),&members,&usage,&resolved.as_ref().map_or(json!([]),|r|r["matched"].clone()),manual.as_ref().and(resolved.as_ref()))?;
            if let Some(r)=&resolved {
                for account in split["accounts"]
                    .as_array_mut()
                    .ok_or_else(Error::unavailable)?
                {
                    if let Some(summary)=array(&r["manual_summary"]["accounts"]).iter().find(|s|s["account_id"]==account["account_id"]) {
                        account["manual_amount"]=summary["amount"].clone();
                    }
                }
            }
            let has_bill=resolved.is_some();
            let eligible=has_bill&&flag(&split,"can_confirm")&&(manual.is_none()||gaps.is_empty());
            let rules=if manual.is_some(){"vendor-r5-manual-v1"}else{"vendor-r5-v1"};
            let rules_digest=digest(&json!({"rules_version":rules,"period_id":pid,"members":members.iter().map(|c|json!([c["id"],c["account_id"]])).collect::<Vec<_>>(),"cost_fen":period["cost_fen"],"creditor_id":period["creditor_id"],"start":period["start"],"end":end,"usage_version":digest(&usage),"coverage":gaps,"bill":resolved.as_ref().map(|r|&r["digest"])}));
            let cost_fen = int(&period,"cost_fen");
            let exact_cost = Decimal::parse(&format!("{}.{:02}", cost_fen / 100, cost_fen % 100))?;
            let bill_difference = resolved.as_ref().filter(|r|!r["total"].is_null()).map(|r| Decimal::parse(text(r,"total")).map(|t| exact_cost.subtract(&t))).transpose()?;
            let difference=bill_difference.as_ref().is_some_and(|d|d.rational()!=Decimal::zero().rational())||resolved.as_ref().is_some_and(|r|int(r,"unmatched_count")>0)||!gaps.is_empty();
            let valid=eligible&&!pending_manual&&period["bill_end"]==end&&period["bill_digest"]==rules_digest&&(!difference||flag(&period,"bill_difference_confirmed"));
            let group=json!({"id":pid,"vendor_id":period["vendor_id"],"name":period["name"],"version":period["version"],"cost_fen":period["cost_fen"],"creditor_id":period["creditor_id"],"start":period["start"],"end":end,"start_confirmed":flag(&period,"start_confirmed"),"member_ids":members.iter().map(|c|c["id"].clone()).collect::<Vec<_>>(),"basis":split["basis"],"bill_import_id":period["bill_import_id"],"pending_bill":!valid,"manual_basis_id":period["manual_basis_id"]});
            let mut calculations=Vec::new();
            for c in &members {
                let a=array(&split["accounts"]).iter().find(|a|a["cycle_id"]==c["id"]).ok_or_else(Error::unavailable)?;
                let own_usage=usage.iter().filter(|u|u.root_id==int(c,"account_id")).collect::<Vec<_>>();
                let mut cycle=c.clone();cycle["cost_fen"]=if valid{a["amount_fen"].clone()}else{Value::Null};
                let mut allocations=if valid{a["allocations"].clone()}else{json!([])};
                Self::apply_names(&mut allocations,&names);
                let mut calc=merge(json!({"cycle":cycle,"end":end,"vendor_period":group,"total_consumption":a["total_consumption"],"allocations":allocations,"record_count":own_usage.len(),"digest":rules_digest,"can_settle":valid&&flag(&period,"start_confirmed"),"allocation_pending":!valid,"data_quality":if gaps.is_empty(){"complete"}else{"upstream_difference"},"coverage_warnings":gaps}),super::cycles::start_meta(c));
                if !valid{calc["cost_warning"]=json!("请导入或重新确认本窗口的账号组账单");}
                if let Some(r)=&resolved {
                    // Members share one import; snapshots retain its identity and the member split.
                    calc["bill"]=merge(without(r.clone(),&["matched","unmatched","outside","weights","local_unmatched","manual_weights","manual_basis"]),json!({"rules_version":rules,"billable_cost_fen":if valid{a["amount_fen"].clone()}else{Value::Null},"import_id":period["bill_import_id"],"file_hash":stored.as_ref().map(|b|&b["file_hash"]),"basis":split["basis"],"group_cost_fen":period["cost_fen"]}));
                }
                if include_records{calc["records"]=serde_json::to_value(own_usage)?;
                    if members.first().is_some_and(|m|m["id"]==c["id"]){calc["vendor_basis"]=json!({"bill":resolved,"split":split,"versions":versions});}
                }
                calculations.push(calc);
            }
            for a in split["accounts"].as_array_mut().ok_or_else(Error::unavailable)?{Self::apply_names(&mut a["allocations"],&names);}
            let result=merge(resolved.clone().unwrap_or(json!({})),json!({"vendor_period":group,"version":period["version"],"digest":rules_digest,"rules_version":rules,"cost_fen":period["cost_fen"],"bill_total_fen":resolved.as_ref().map(|r|if manual.is_some(){r["upstream_total_fen"].clone()}else{r["cost_fen"].clone()}),"billable_cost_fen":period["cost_fen"],"accounts":split["accounts"],"basis":split["basis"],"can_confirm":eligible,"difference_required":difference,"difference_amount":bill_difference.map(|d|d.to_string()),"coverage_warnings":gaps,"end":end}));
            Ok(json!({"result":result,"versions":versions,"period":period,"calculations":calculations}))
        }).await.map_err(|_|Error::unavailable())?
    }
    pub async fn verify_vendor_prepared(db: &mut Db, p: &Value) -> Result<()> {
        let current = one(
            db,
            "SELECT * FROM vendor_periods WHERE id=?",
            &[p["period"]["id"].clone()],
        )
        .await?;
        if current["status"] != "open" || current["version"] != p["period"]["version"] {
            return Err(Error::conflict("账号组周期已变化，请重新预览"));
        }
        let members = Self::vendor_members(db, int(&current, "id")).await?;
        if members.len() != array(&p["versions"]).len() {
            return Err(Error::conflict("成员已变化，请重新预览"));
        }
        for v in array(&p["versions"]) {
            if !members
                .iter()
                .any(|c| c["id"] == v["cycle_id"] && c["version"] == v["version"])
                || store::revision(db, int(v, "account_id")).await? != int(v, "usage_version")
            {
                return Err(Error::conflict("成员或消费记录已变化，请重新预览"));
            }
        }
        Ok(())
    }
    pub async fn vendor_bill_preview(
        &self,
        actor: &Value,
        pid: i64,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        if data["allocation_mode"]
            .as_str()
            .is_some_and(|m| m != "proportional")
        {
            return Err(Error::invalid("账号组账单只支持比例分摊"));
        }
        let end = time::instant(text(data, "end"))?;
        let mut db = self.store.read().await?;
        let ids = Self::vendor_members(&mut db, pid)
            .await?
            .iter()
            .map(|c| c["id"].clone())
            .collect::<Vec<_>>();
        drop(db);
        self.sync_selection(&ids, &end, None).await?;
        let mut db = self.store.read().await?;
        Self::fresh(&mut db).await?;
        let period = one(
            &mut db,
            "SELECT * FROM vendor_periods WHERE id=?",
            &[json!(pid)],
        )
        .await?;
        if period["version"] != data["version"] || period["status"] != "open" {
            return Err(Error::conflict("账号组周期已变化，请刷新"));
        }
        if end > time::now()
            || end.as_str() <= text(&period, "start")
            || period["end"].as_str().is_some_and(|e| e != end)
        {
            return Err(Error::invalid("账单截止时间无效"));
        }
        let manual = data
            .get("manual_basis")
            .map(crate::manual_billing::normalize)
            .transpose()?;
        let local_only = manual.as_ref().is_some_and(|m| m["source"] == "local_only");
        if local_only && (!text(data, "csv").is_empty() || !data["import_id"].is_null()) {
            return Err(Error::invalid("仅用本地记录时不能同时上传或沿用账单"));
        }
        let (rows, file_name, file_hash) = if local_only {
            (Vec::new(), Value::Null, Value::Null)
        } else if !data["import_id"].is_null() {
            let b = one(
                &mut db,
                "SELECT * FROM vendor_bill_imports WHERE id=? AND vendor_period_id=?",
                &[data["import_id"].clone(), json!(pid)],
            )
            .await?;
            (
                serde_json::from_str::<Vec<BillRow>>(text(&b, "rows"))?,
                b["file_name"].clone(),
                b["file_hash"].clone(),
            )
        } else {
            let csv = text(data, "csv").to_owned();
            let hash = json!(crate::json::hash(csv.as_bytes()));
            let filename = json!(
                text(data, "file_name")
                    .replace('\\', "/")
                    .rsplit('/')
                    .next()
                    .unwrap_or("usage.csv")
            );
            let permit = self
                .cpu
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| Error::unavailable())?;
            let rows = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                billing::parse_csv_rows(&csv)
            })
            .await
            .map_err(|_| Error::unavailable())??;
            (rows, filename, hash)
        };
        let row_values = serde_json::to_value(&rows)?;
        let mut prepared = self
            .vendor_calculation_basis(&mut db, pid, &end, Some(rows), false, manual)
            .await?;
        prepared["rows"] = row_values;
        prepared["result"]["file_name"] = file_name;
        prepared["result"]["file_hash"] = file_hash;
        db.commit().await?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let expires = time::after(600);
        let mut write = self.store.write().await?;
        Self::verify_vendor_prepared(&mut write, &prepared).await?;
        exec(
            &mut write,
            "DELETE FROM vendor_bill_previews WHERE expires_at<?",
            &[json!(time::now())],
        )
        .await?;
        if !prepared["result"]["manual_basis"].is_null()
            || !prepared["period"]["manual_basis_id"].is_null()
            || Self::has_manual_draft(&mut write, None, Some(pid)).await?
        {
            exec(
                &mut write,
                "DELETE FROM vendor_bill_previews WHERE vendor_period_id=?",
                &[json!(pid)],
            )
            .await?;
            Self::mark_manual_draft(&mut write, None, Some(pid)).await?;
        }
        exec(
            &mut write,
            "INSERT INTO vendor_bill_previews VALUES(?,?,?,?,?)",
            &[
                json!(id),
                actor["id"].clone(),
                json!(pid),
                json!(dumps(&prepared)),
                json!(expires),
            ],
        )
        .await?;
        write.commit().await?;
        Ok(merge(
            prepared["result"].clone(),
            json!({"id":id,"expires_at":expires}),
        ))
    }
    pub async fn prepare_vendor_bill(
        &self,
        actor: &Value,
        pid: i64,
        data: &Value,
    ) -> Result<Value> {
        admin(actor)?;
        let mut db = self.store.read().await?;
        let p = one(
            &mut db,
            "SELECT * FROM vendor_bill_previews WHERE id=? AND actor_id=? AND vendor_period_id=?",
            &[data["preview_id"].clone(), actor["id"].clone(), json!(pid)],
        )
        .await?;
        if text(&p, "expires_at") <= time::now().as_str() {
            return Err(Error::conflict("账单预览已过期"));
        }
        let p: Value = serde_json::from_str(text(&p, "payload"))?;
        let ids = array(&p["versions"])
            .iter()
            .map(|v| v["cycle_id"].clone())
            .collect::<Vec<_>>();
        let end = text(&p["result"], "end").to_owned();
        drop(db);
        self.sync_selection(&ids, &end, None).await?;
        let mut db = self.store.read().await?;
        Self::fresh(&mut db).await?;
        Self::verify_vendor_prepared(&mut db, &p).await?;
        self.vendor_calculation_basis(
            &mut db,
            pid,
            &end,
            Some(serde_json::from_value(p["rows"].clone())?),
            false,
            p["result"].get("manual_basis").cloned(),
        )
        .await
    }
    pub async fn confirm_vendor_bill(
        &self,
        db: &mut Db,
        actor: &Value,
        pid: i64,
        data: &Value,
        prepared: Option<Value>,
    ) -> Result<Value> {
        admin(actor)?;
        Self::fresh(db).await?;
        let saved = one(
            db,
            "SELECT * FROM vendor_bill_previews WHERE id=? AND actor_id=? AND vendor_period_id=?",
            &[data["preview_id"].clone(), actor["id"].clone(), json!(pid)],
        )
        .await?;
        if text(&saved, "expires_at") <= time::now().as_str() {
            return Err(Error::conflict("账单预览已过期"));
        }
        let p: Value = serde_json::from_str(text(&saved, "payload"))?;
        let fresh = prepared.ok_or_else(Error::unavailable)?;
        Self::verify_vendor_prepared(db, &fresh).await?;
        if p["period"]["version"] != data["version"]
            || p["result"]["digest"] != fresh["result"]["digest"]
            || p["period"]["version"] != fresh["period"]["version"]
        {
            return Err(Error::conflict("账单依据已变化，请重新预览"));
        }
        let r = &fresh["result"];
        if !flag(r, "can_confirm") {
            return Err(Error::invalid("没有正金额匹配或本地消费依据，无法分摊"));
        }
        if !flag(&fresh["period"], "start_confirmed") {
            return Err(Error::conflict("请先确认账号组起点"));
        }
        if flag(r, "difference_required") && !flag(data, "accept_difference") {
            return Err(Error::invalid("请确认账单总额、未匹配费用与历史覆盖差异"));
        }
        if int(r, "outside_count") > 0 && !flag(data, "accept_outside") {
            return Err(Error::invalid("请确认排除周期范围外记录"));
        }
        let local_only = r["manual_basis"]["source"] == "local_only";
        let b = if local_only {
            json!({"id":null})
        } else {
            exec(db,"INSERT INTO vendor_bill_imports(vendor_period_id,file_name,file_hash,rows,created_by,created_at) VALUES(?,?,?,?,?,?) ON CONFLICT(vendor_period_id,file_hash) DO NOTHING",&[json!(pid),p["result"]["file_name"].clone(),p["result"]["file_hash"].clone(),json!(dumps(&p["rows"])),actor["id"].clone(),json!(time::now())]).await?;
            one(
                db,
                "SELECT id FROM vendor_bill_imports WHERE vendor_period_id=? AND file_hash=?",
                &[json!(pid), p["result"]["file_hash"].clone()],
            )
            .await?
        };
        let basis_id = Self::save_manual_basis(db, actor, None, Some(pid), r).await?;
        Self::clear_manual_draft(db, None, Some(pid)).await?;
        exec(
            db,
            "UPDATE vendor_periods SET manual_basis_id=? WHERE id=?",
            &[basis_id, json!(pid)],
        )
        .await?;
        exec(db,"UPDATE vendor_periods SET bill_import_id=?,bill_end=?,bill_digest=?,bill_difference_confirmed=?,version=version+1 WHERE id=?",&[b["id"].clone(),r["end"].clone(),r["digest"].clone(),json!(flag(data,"accept_difference") as i64),json!(pid)]).await?;
        for a in array(&r["accounts"]) {
            exec(db,"UPDATE cycles SET cost_fen=?,bill_end=?,cost_source='vendor_bill',version=version+1 WHERE id=? AND vendor_period_id=?",&[a["amount_fen"].clone(),r["end"].clone(),a["cycle_id"].clone(),json!(pid)]).await?;
        }
        exec(
            db,
            "DELETE FROM vendor_bill_previews WHERE vendor_period_id=?",
            &[json!(pid)],
        )
        .await?;
        audit(db,actor,"confirm_vendor_bill",json!(pid),&json!({"import_id":b["id"],"file_hash":p["result"]["file_hash"],"digest":r["digest"],"cost_fen":r["cost_fen"],"bill_total_fen":r["bill_total_fen"],"basis":r["basis"],"accept_difference":data["accept_difference"],"accept_outside":data["accept_outside"]})).await?;
        Ok(json!({"id":pid,"version":int(&fresh["period"],"version")+1,"import_id":b["id"]}))
    }
    pub async fn vendor_unmatched_csv(
        &self,
        actor: &Value,
        pid: i64,
        preview: &str,
    ) -> Result<Vec<u8>> {
        admin(actor)?;
        let mut db = self.store.read().await?;
        let p = one(
            &mut db,
            "SELECT * FROM vendor_bill_previews WHERE id=? AND actor_id=? AND vendor_period_id=?",
            &[json!(preview), actor["id"].clone(), json!(pid)],
        )
        .await?;
        if text(&p, "expires_at") <= time::now().as_str() {
            return Err(Error::conflict("账单预览已过期，请重新匹配"));
        }
        let p: Value = serde_json::from_str(text(&p, "payload"))?;
        Self::verify_vendor_prepared(&mut db, &p).await?;
        if !p["result"]["manual_basis"].is_null() {
            return super::operations::manual_basis_csv(&p["result"]);
        }
        super::operations::csv_bytes(
            &[
                "CSV 行号",
                "请求时间（UTC）",
                "模型",
                "请求标识",
                "上游扣费（元，原始精度）",
                "未匹配原因",
            ],
            array(&p["result"]["unmatched"])
                .iter()
                .map(|r| (r, false))
                .chain(array(&p["result"]["outside"]).iter().map(|r| (r, true)))
                .map(|(r, outside)| {
                    vec![
                        int(r, "row").to_string(),
                        text(r, "created_at").to_owned(),
                        text(r, "model").to_owned(),
                        text(r, "request_id").to_owned(),
                        text(r, "amount").to_owned(),
                        if outside {
                            "周期范围外"
                        } else if r["reason"] == "ambiguous" {
                            "存在歧义"
                        } else {
                            "未找到本地请求"
                        }
                        .to_owned(),
                    ]
                }),
        )
    }
}
