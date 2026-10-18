use super::*;
use crate::{
    billing::{self, BillRow, RequestDetail, UsageRow, bill_allocations},
    json::{array, flag, merge, without},
    money::{Decimal, allocate},
    service::cycles::{estimate, start_meta},
    store::{all, exec},
};

impl Service {
    pub(super) async fn names(db: &mut Db) -> Result<BTreeMap<i64, String>> {
        Ok(all(db, "SELECT id,name FROM users", &[])
            .await?
            .into_iter()
            .map(|u| (int(&u, "id"), text(&u, "name").to_owned()))
            .collect())
    }
    pub(super) fn apply_names(shares: &mut Value, names: &BTreeMap<i64, String>) {
        if let Some(a) = shares.as_array_mut() {
            for s in a {
                let uid = int(s, "user_id");
                s["user_name"] = json!(
                    names
                        .get(&uid)
                        .cloned()
                        .unwrap_or_else(|| format!("用户 #{uid}"))
                );
            }
        }
    }
    fn consumption_allocations<'a>(
        rows: impl Iterator<Item = (i64, &'a str)>,
        c: &Value,
        names: &BTreeMap<i64, String>,
    ) -> Result<(Decimal, Value)> {
        let mut weights: BTreeMap<i64, Decimal> = BTreeMap::new();
        for (user_id, amount) in rows {
            let weight = weights.entry(user_id).or_insert_with(Decimal::zero);
            weight.add(&Decimal::parse(amount)?);
            *weight = weight.rounded(60);
        }
        let mut total = Decimal::zero();
        for weight in weights.values() {
            total.add(weight);
            total = total.rounded(60);
        }
        let mut shares = allocate(int(c, "cost_fen"), &weights, int(c, "creditor_id"), 60)?;
        Self::apply_names(&mut shares, names);
        Ok((total, shares))
    }
    pub async fn overview_calculation(&self, db: &mut Db, c: &Value, end: &str) -> Result<Value> {
        if !c["manual_basis_id"].is_null()
            || !c["vendor_period_id"].is_null()
            || (c["cost_source"] == "upstream_bill" && int(c, "bill_import_id") != 0)
        {
            return self.preview_calculation(db, c, end).await;
        }
        let permit = self
            .cpu
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::unavailable())?;
        // The overview does not expose a usage digest or snapshot. Read only the
        // two fields needed for the same ordered, full-precision allocation.
        use futures_util::TryStreamExt;
        use sqlx::Row;
        let mut rows: Vec<(i64, String)> = Vec::new();
        {
            let mut stream = sqlx::query("SELECT user_id,amount FROM usage WHERE root_id=? AND created_at>=? AND created_at<? ORDER BY id")
                .bind(int(c,"account_id")).bind(text(c,"start")).bind(end).fetch(&mut *db);
            while let Some(row) = stream.try_next().await? {
                rows.push((row.try_get(0)?, row.try_get(1)?));
            }
        }
        let names = Self::names(db).await?;
        let c = c.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let (total, shares) = Self::consumption_allocations(rows.iter().map(|(id, amount)| (*id, amount.as_str())), &c, &names)?;
            let confirmed = c.get("start_confirmed").map(|_| flag(&c, "start_confirmed")).unwrap_or(true);
            let mut result = json!({"total_consumption":total.to_string(),"allocations":shares,"record_count":rows.len(),"can_settle":confirmed});
            if c.get("cost_kind").is_some_and(|kind| kind != "exact") {
                result["can_settle"] = json!(false);
                result["cost_warning"] = json!("请先确认准确成本或导入上游账单");
            }
            Ok(result)
        }).await.map_err(|_| Error::unavailable())?
    }
    pub(super) async fn usage(db: &mut Db, c: &Value, end: &str) -> Result<Vec<UsageRow>> {
        use futures_util::TryStreamExt;
        use sqlx::Row;
        let mut stream = sqlx::query("SELECT account_id,amount,created_at,id,root_id,user_id FROM usage WHERE root_id=? AND created_at>=? AND created_at<? ORDER BY id")
            .bind(int(c,"account_id")).bind(text(c,"start")).bind(end).fetch(db);
        let mut rows = Vec::new();
        while let Some(r) = stream.try_next().await? {
            rows.push(UsageRow {
                account_id: r.try_get(0)?,
                amount: r.try_get(1)?,
                created_at: r.try_get(2)?,
                id: r.try_get(3)?,
                root_id: r.try_get(4)?,
                user_id: r.try_get(5)?,
            });
        }
        Ok(rows)
    }
    pub(super) async fn details(
        db: &mut Db,
        c: &Value,
        end: &str,
    ) -> Result<BTreeMap<i64, RequestDetail>> {
        use futures_util::TryStreamExt;
        use sqlx::Row;
        let mut stream = sqlx::query("SELECT d.usage_id,d.payload FROM usage_details d JOIN usage u ON u.id=d.usage_id WHERE u.root_id=? AND u.created_at>=? AND u.created_at<?")
            .bind(int(c,"account_id")).bind(text(c,"start")).bind(end).fetch(db);
        let mut details = BTreeMap::new();
        while let Some(r) = stream.try_next().await? {
            details.insert(
                r.try_get(0)?,
                serde_json::from_str(r.try_get::<&str, _>(1)?)?,
            );
        }
        Ok(details)
    }
    pub async fn calculation(&self, db: &mut Db, c: &Value, end: &str) -> Result<Value> {
        self.calculate(db, c, end, true).await
    }
    pub async fn preview_calculation(&self, db: &mut Db, c: &Value, end: &str) -> Result<Value> {
        self.calculate(db, c, end, false).await
    }
    async fn calculate(
        &self,
        db: &mut Db,
        c: &Value,
        end: &str,
        include_records: bool,
    ) -> Result<Value> {
        if let Some(pid) = c["vendor_period_id"].as_i64() {
            let group = self
                .vendor_calculation(db, pid, end, None, include_records)
                .await?;
            return array(&group["calculations"])
                .iter()
                .find(|v| v["cycle"]["id"] == c["id"])
                .cloned()
                .ok_or_else(Error::unavailable);
        }
        let permit = self
            .cpu
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::unavailable())?;
        let rows = Self::usage(db, c, end).await?;
        let names = Self::names(db).await?;
        let gap = Self::coverage_error(db, c, end).await?;
        let bill = if c["cost_source"] == "upstream_bill" && int(c, "bill_import_id") != 0 {
            Some(
                one(
                    db,
                    "SELECT * FROM bill_imports WHERE id=? AND cycle_id=?",
                    &[c["bill_import_id"].clone(), c["id"].clone()],
                )
                .await?,
            )
        } else {
            None
        };
        let manual = Self::manual_config(db, c).await?;
        let pending_manual = Self::has_manual_draft(db, Some(int(c, "id")), None).await?;
        let details = if bill.is_some() || manual.is_some() {
            Self::details(db, c, end).await?
        } else {
            BTreeMap::new()
        };
        let c = c.clone();
        let end = end.to_owned();
        tokio::task::spawn_blocking(move||{
            let _permit=permit;
            let (total,shares)=Self::consumption_allocations(rows.iter().map(|r|(r.user_id,r.amount.as_str())),&c,&names)?;
            let row_digest=digest(&rows);let confirmed=c.get("start_confirmed").map(|_|flag(&c,"start_confirmed")).unwrap_or(true);
            let mut result=merge(json!({"cycle":c,"end":end,"total_consumption":total.to_string(),"allocations":shares,"digest":row_digest,"record_count":rows.len(),"data_quality":if confirmed{"complete"}else{"estimated"},"can_settle":confirmed}),start_meta(&c));
            if c.get("cost_kind").is_some_and(|k|k!="exact"){result["can_settle"]=json!(false);result["cost_warning"]=json!("请先确认准确成本或导入上游账单");}
            if bill.is_some() || manual.is_some(){
                let bill = bill.unwrap_or(json!({"rows":"[]"}));
                let bill_rows:Vec<BillRow>=serde_json::from_str(text(&bill,"rows"))?;
                let mode=c["bill_allocation_mode"].as_str().unwrap_or("proportional");
                let resolved=if let Some(config)=&manual {crate::manual_billing::reconcile(&bill_rows,&rows,&details,text(&c,"start"),&end,mode,config,&[int(&c,"account_id")])?}else{billing::reconcile(&bill_rows,&rows,&details,text(&c,"start"),&end,mode)?};
                let mut allocations=bill_allocations(&resolved,int(&c,"creditor_id"))?;Self::apply_names(&mut allocations,&names);
                result["allocations"]=allocations;result["digest"]=json!(digest(&json!([row_digest,resolved["digest"],bill["file_hash"]])));
                let valid=c["bill_end"]==end&&c["bill_digest"]==resolved["digest"]&&c["cost_fen"]==resolved["cost_fen"];
                if !valid{result["can_settle"]=json!(false);result["cost_warning"]=json!("账单范围或匹配结果已变化，请重新确认账单");}
                if if manual.is_some(){!flag(&resolved,"can_confirm")}else{!Decimal::parse(text(&resolved,"matched_amount"))?.positive()}{result["can_settle"]=json!(false);result["cost_warning"]=json!("没有正金额匹配依据，无法分摊");}
                if mode=="proportional"&&(int(&resolved,"unmatched_count")!=0||gap.is_some())&&!flag(&c,"bill_difference_confirmed"){result["can_settle"]=json!(false);result["cost_warning"]=json!("请确认上游账单与本地记录的差异");}
                if gap.is_some()||int(&resolved,"unmatched_count")!=0{result["data_quality"]=json!("upstream_difference");}
                if manual.is_some() && gap.is_some(){result["can_settle"]=json!(false);result["cost_warning"]=json!("补计不能绕过本地消费覆盖缺口");}
                result["bill"]=merge(resolved,json!({"file_name":bill["file_name"],"file_hash":bill["file_hash"],"import_id":bill["id"]}));
            }
            if pending_manual {result["can_settle"]=json!(false);result["cost_warning"]=json!("人工计费依据已修改，请重新确认账单依据");}
            if include_records { result["records"]=serde_json::to_value(rows)?; }
            Ok(result)
        }).await.map_err(|_|Error::unavailable())?
    }
    pub async fn expense_calculation(&self, db: &mut Db, e: &Value) -> Result<Value> {
        let permit = self
            .cpu
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::unavailable())?;
        let mut e = e.clone();
        if let Some(s) = e["members"].as_str() {
            e["members"] = serde_json::from_str(s)?;
        }
        let names = Self::names(db).await?;
        let creditor_name = name(db, int(&e, "creditor_id")).await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let weights = array(&e["members"])
                .iter()
                .map(|m| {
                    Ok((
                        int(m, "user_id"),
                        Decimal::parse(&int(m, "weight").to_string())?,
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            let mut shares = allocate(int(&e, "cost_fen"), &weights, int(&e, "creditor_id"), 28)?;
            let items = shares.as_array_mut().ok_or_else(Error::unavailable)?;
            for member in array(&e["members"]) {
                if int(member, "weight") == 0 {
                    items.push(json!({"user_id":member["user_id"],"consumption":"0","amount_fen":0,"share":"0","self_covered":member["user_id"]==e["creditor_id"]}));
                }
            }
            items.sort_by_key(|a| int(a, "user_id"));
            Self::apply_names(&mut shares, &names);
            Ok(json!({"source_type":"fixed_expense","expense":e,"allocations":shares,"can_settle":true,"creditor_name":creditor_name,"digest":digest(&e)}))
        })
        .await
        .map_err(|_| Error::unavailable())?
    }
    pub fn redact(mut result: Value, actor: &Value) -> Value {
        result = without(result, &["records"]);
        if text(actor, "role") != "admin" {
            result = without(result, &["vendor_basis"]);
            result["allocations"] = json!(
                array(&result["allocations"])
                    .iter()
                    .filter(|a| a["user_id"] == actor["id"])
                    .cloned()
                    .collect::<Vec<_>>()
            );
            if result.get("expense").is_some() {
                result["expense"] = without(result["expense"].clone(), &["members"]);
            }
            if result.get("bill").is_some_and(|b| !b.is_null()) {
                result["bill"] = without(
                    result["bill"].clone(),
                    &[
                        "weights",
                        "matched",
                        "unmatched",
                        "outside",
                        "file_name",
                        "file_hash",
                        "import_id",
                        "manual_weights",
                        "local_unmatched",
                        "manual_basis",
                    ],
                );
                if let Some(summary) = result["bill"]["manual_summary"].as_object_mut() {
                    summary.remove("accounts");
                }
            }
        }
        result
    }
    pub async fn preview(&self, actor: &Value, data: &Value) -> Result<Value> {
        admin(actor)?;
        let ids = array(&data["ids"]);
        let end = time::instant(text(data, "end"))?;
        if end > time::now() {
            return Err(Error::invalid("结束时间不能晚于现在"));
        }
        if !ids.is_empty() {
            self.sync_selection(ids, &end, None).await?;
        }
        let mut db = self.store.read().await?;
        if !ids.is_empty() {
            Self::fresh(&mut db).await?;
        }
        Self::complete_vendor_selection(&mut db, ids).await?;
        let mut vendor_cache = BTreeMap::<i64, Value>::new();
        let mut cycles = Vec::new();
        let mut versions = Vec::new();
        let mut expenses = Vec::new();
        for cid in ids {
            let c = one(
                &mut db,
                "SELECT * FROM cycles WHERE id=?",
                std::slice::from_ref(cid),
            )
            .await?;
            if c["status"] != "open" {
                return Err(Error::conflict("所选周期已结算，请刷新"));
            }
            let actual_end = c["end"].as_str().unwrap_or(&end);
            if actual_end <= text(&c, "start") {
                return Err(Error::invalid("结束时间必须晚于开始时间"));
            }
            let gap = Self::coverage_error(&mut db, &c, actual_end).await?;
            if let Some(gap) = &gap
                && !estimate(text(&c, "start_mode"))
                && c["cost_source"] != "upstream_bill"
                && c["cost_kind"] != "estimated"
                && c["vendor_period_id"].is_null()
            {
                return Err(Error::conflict(gap));
            }
            let mut calc = if let Some(pid) = c["vendor_period_id"].as_i64() {
                if let std::collections::btree_map::Entry::Vacant(entry) = vendor_cache.entry(pid) {
                    entry.insert(
                        self.vendor_calculation(&mut db, pid, actual_end, None, false)
                            .await?,
                    );
                }
                array(&vendor_cache[&pid]["calculations"])
                    .iter()
                    .find(|v| v["cycle"]["id"] == c["id"])
                    .cloned()
                    .ok_or_else(Error::unavailable)?
            } else {
                self.preview_calculation(&mut db, &c, actual_end).await?
            };
            if let Some(gap) = gap {
                if c["cost_source"] != "upstream_bill" && c["vendor_period_id"].is_null() {
                    calc["data_quality"] = json!("incomplete");
                    calc["can_settle"] = json!(false);
                    calc["start_warning"] = json!(gap);
                }
            } else if !flag(&calc, "start_confirmed") {
                calc["data_quality"] = json!("estimated");
                calc["can_settle"] = json!(false);
                calc["start_warning"] = json!("周期起点未确认");
            }
            calc = without(calc, &["records"]);
            if let Some(pid) = c["vendor_period_id"].as_i64() {
                let period = &vendor_cache[&pid]["period"];
                if !period["manual_basis_id"].is_null() {
                    calc["vendor_period"]["manual_basis"] =
                        json!(Self::manual_config(&mut db, period).await?);
                }
            }
            calc["creditor_name"] = json!(name(&mut db, int(&c, "creditor_id")).await?);
            versions.push((
                int(&c, "id"),
                int(&c, "version"),
                int(&c, "account_id"),
                store::revision(&mut db, int(&c, "account_id")).await?,
            ));
            cycles.push(calc);
        }
        for eid in array(&data["expense_ids"]) {
            let e = one(
                &mut db,
                "SELECT * FROM expenses WHERE id=?",
                std::slice::from_ref(eid),
            )
            .await?;
            if e["status"] != "open" {
                return Err(Error::conflict("固定费用已出单，请刷新"));
            }
            expenses.push(self.expense_calculation(&mut db, &e).await?);
        }
        db.commit().await?;
        let payload = dumps(&cycles.iter().chain(expenses.iter()).collect::<Vec<_>>());
        let mut write = self.store.write().await?;
        for (cid, v, root, rev) in versions {
            let c = one(
                &mut write,
                "SELECT version FROM cycles WHERE id=?",
                &[json!(cid)],
            )
            .await?;
            if int(&c, "version") != v || store::revision(&mut write, root).await? != rev {
                return Err(Error::conflict("消费或周期已更新，请重新预览"));
            }
        }
        for calc in &expenses {
            let e = one(
                &mut write,
                "SELECT version FROM expenses WHERE id=?",
                &[calc["expense"]["id"].clone()],
            )
            .await?;
            if e["version"] != calc["expense"]["version"] {
                return Err(Error::conflict("固定费用已修改，请重新预览"));
            }
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let expiry = time::after(600);
        exec(
            &mut write,
            "DELETE FROM previews WHERE expires_at<?",
            &[json!(time::now())],
        )
        .await?;
        exec(
            &mut write,
            "INSERT INTO previews VALUES(?,?,?,?)",
            &[
                json!(id),
                actor["id"].clone(),
                Value::String(payload),
                json!(expiry),
            ],
        )
        .await?;
        write.commit().await?;
        for calculation in &mut cycles {
            *calculation = self.present_calculation(calculation.take(), false).await;
        }
        Ok(json!({"id":id,"cycles":cycles,"expenses":expenses,"expires_at":expiry}))
    }
    pub async fn bill_preview(&self, actor: &Value, cid: i64, data: &Value) -> Result<Value> {
        admin(actor)?;
        let end = time::instant(text(data, "end"))?;
        self.sync_selection(&[json!(cid)], &end, None).await?;
        let mut db = self.store.read().await?;
        Self::fresh(&mut db).await?;
        let c = one(&mut db, "SELECT * FROM cycles WHERE id=?", &[json!(cid)]).await?;
        Self::independent_cycle(&c).await?;
        if c["status"] != "open" || c["version"] != data["version"] {
            return Err(Error::conflict("周期已修改或已出单，请重新预览"));
        }
        if end > time::now()
            || end.as_str() <= text(&c, "start")
            || c["end"].as_str().is_some_and(|v| v != end)
        {
            return Err(Error::invalid("账单截止时间无效"));
        }
        let manual = data
            .get("manual_basis")
            .map(crate::manual_billing::normalize)
            .transpose()?;
        let local_only = manual.as_ref().is_some_and(|m| m["source"] == "local_only");
        if local_only && (!text(data, "csv").is_empty() || int(data, "import_id") != 0) {
            return Err(Error::invalid("仅用本地记录时不能同时上传或沿用账单"));
        }
        let (rows, file_name, file_hash) = if local_only {
            (Vec::new(), String::new(), String::new())
        } else if int(data, "import_id") != 0 {
            let b = one(
                &mut db,
                "SELECT * FROM bill_imports WHERE id=? AND cycle_id=?",
                &[data["import_id"].clone(), json!(cid)],
            )
            .await?;
            (
                serde_json::from_str(text(&b, "rows"))?,
                text(&b, "file_name").to_owned(),
                text(&b, "file_hash").to_owned(),
            )
        } else {
            let csv = text(data, "csv").to_owned();
            let hash = crate::json::hash(csv.as_bytes());
            let file_name = if text(data, "file_name").is_empty() {
                "usage.csv"
            } else {
                text(data, "file_name")
            }
            .replace('\\', "/")
            .rsplit('/')
            .next()
            .unwrap_or("usage.csv")
            .chars()
            .take(200)
            .collect();
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
            (rows, file_name, hash)
        };
        let rows = Arc::new(rows);
        let revision = store::revision(&mut db, int(&c, "account_id")).await?;
        let mut result = self
            .reconcile_db(
                &mut db,
                &c,
                &end,
                rows.clone(),
                manual.clone(),
                data["allocation_mode"]
                    .as_str()
                    .unwrap_or(c["bill_allocation_mode"].as_str().unwrap_or("proportional")),
            )
            .await?;
        let gap = Self::coverage_error(&mut db, &c, &end).await?;
        let metadata = json!({"file_name":file_name,"file_hash":file_hash,"end":end,"version":c["version"],"difference_required":result["allocation_mode"]=="proportional"&&(int(&result,"unmatched_count")!=0||gap.is_some()),"coverage_warning":gap,"can_confirm":if manual.is_some(){flag(&result,"can_confirm") && gap.is_none() && flag(&c,"start_confirmed")}else{Decimal::parse(text(&result,"matched_amount"))?.positive()}});
        result = merge(result, metadata);
        db.commit().await?;
        #[derive(serde::Serialize)]
        struct Payload<'a> {
            result: &'a Value,
            rows: &'a [BillRow],
        }
        let payload = dumps(&Payload {
            result: &result,
            rows: &rows,
        });
        let id = uuid::Uuid::new_v4().simple().to_string();
        let mut db = self.store.write().await?;
        let current = one(
            &mut db,
            "SELECT version FROM cycles WHERE id=?",
            &[json!(cid)],
        )
        .await?;
        if current["version"] != c["version"]
            || store::revision(&mut db, int(&c, "account_id")).await? != revision
        {
            return Err(Error::conflict("请求或周期已更新，请重新预览"));
        }
        exec(
            &mut db,
            "DELETE FROM bill_previews WHERE expires_at<?",
            &[json!(time::now())],
        )
        .await?;
        if manual.is_some()
            || !c["manual_basis_id"].is_null()
            || Self::has_manual_draft(&mut db, Some(cid), None).await?
        {
            exec(
                &mut db,
                "DELETE FROM bill_previews WHERE cycle_id=?",
                &[json!(cid)],
            )
            .await?;
            Self::mark_manual_draft(&mut db, Some(cid), None).await?;
        }
        exec(
            &mut db,
            "INSERT INTO bill_previews VALUES(?,?,?,?,?)",
            &[
                json!(id),
                actor["id"].clone(),
                json!(cid),
                Value::String(payload),
                json!(time::after(600)),
            ],
        )
        .await?;
        db.commit().await?;
        Ok(merge(json!({"id":id}), result))
    }
    pub async fn reconcile_db(
        &self,
        db: &mut Db,
        c: &Value,
        end: &str,
        rows: Arc<Vec<BillRow>>,
        manual: Option<Value>,
        mode: &str,
    ) -> Result<Value> {
        let permit = self
            .cpu
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::unavailable())?;
        let usage = Self::usage(db, c, end).await?;
        let details = Self::details(db, c, end).await?;
        let names = Self::names(db).await?;
        let start = text(c, "start").to_owned();
        let end = end.to_owned();
        let mode = mode.to_owned();
        let creditor = int(c, "creditor_id");
        let root = int(c, "account_id");
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut result = if let Some(config) = &manual {
                crate::manual_billing::reconcile(
                    &rows,
                    &usage,
                    &details,
                    &start,
                    &end,
                    &mode,
                    config,
                    &[root],
                )?
            } else {
                billing::reconcile(&rows, &usage, &details, &start, &end, &mode)?
            };
            let mut shares = bill_allocations(&result, creditor)?;
            Self::apply_names(&mut shares, &names);
            result["allocations"] = shares;
            Ok(result)
        })
        .await
        .map_err(|_| Error::unavailable())?
    }
    pub async fn prepare_bill(
        &self,
        actor: &Value,
        cid: i64,
        data: &Value,
    ) -> Result<(Value, i64)> {
        let mut db = self.store.read().await?;
        let p = store::one(
            &mut db,
            "SELECT payload FROM bill_previews WHERE id=? AND actor_id=? AND cycle_id=?",
            &[data["preview_id"].clone(), actor["id"].clone(), json!(cid)],
            "账单预览不存在",
        )
        .await?;
        let mut p: Value = serde_json::from_str(text(&p, "payload"))?;
        let end = text(&p["result"], "end").to_owned();
        drop(db);
        self.sync_selection(&[json!(cid)], &end, None).await?;
        let mut db = self.store.read().await?;
        let c = one(&mut db, "SELECT * FROM cycles WHERE id=?", &[json!(cid)]).await?;
        let revision = store::revision(&mut db, int(&c, "account_id")).await?;
        let fresh = self
            .reconcile_db(
                &mut db,
                &c,
                &end,
                Arc::new(serde_json::from_value(p["rows"].take())?),
                p["result"].get("manual_basis").cloned(),
                p["result"]["allocation_mode"]
                    .as_str()
                    .unwrap_or("proportional"),
            )
            .await?;
        Ok((fresh, revision))
    }
    pub async fn confirm_bill(
        &self,
        db: &mut Db,
        actor: &Value,
        cid: i64,
        data: &Value,
        prepared: Option<(Value, i64)>,
    ) -> Result<Value> {
        admin(actor)?;
        Self::fresh(db).await?;
        let p = store::one(
            db,
            "SELECT * FROM bill_previews WHERE id=? AND actor_id=? AND cycle_id=?",
            &[data["preview_id"].clone(), actor["id"].clone(), json!(cid)],
            "账单预览不存在",
        )
        .await?;
        if text(&p, "expires_at") < time::now().as_str() {
            return Err(Error::conflict("账单预览已过期"));
        }
        let payload: Value = serde_json::from_str(text(&p, "payload"))?;
        let result = &payload["result"];
        let c = one(db, "SELECT * FROM cycles WHERE id=?", &[json!(cid)]).await?;
        Self::independent_cycle(&c).await?;
        if c["status"] != "open"
            || c["version"] != data["version"]
            || c["version"] != result["version"]
        {
            return Err(Error::conflict("成本或周期已修改，请重新预览"));
        }
        let (fresh, rev) = prepared.ok_or_else(Error::unavailable)?;
        if rev != store::revision(db, int(&c, "account_id")).await?
            || fresh["digest"] != result["digest"]
        {
            return Err(Error::conflict("请求匹配已更新，请重新预览"));
        }
        if if fresh["manual_basis"].is_null() {
            !Decimal::parse(text(&fresh, "matched_amount"))?.positive()
        } else {
            !flag(&fresh, "can_confirm")
        } {
            return Err(Error::invalid("没有正金额匹配依据，无法分摊"));
        }
        let mode = result["allocation_mode"].as_str().unwrap_or("proportional");
        let gap = Self::coverage_error(db, &c, text(result, "end")).await?;
        if !fresh["manual_basis"].is_null() && (gap.is_some() || !flag(&c, "start_confirmed")) {
            return Err(Error::conflict("请先确认起点并解决本地消费覆盖缺口"));
        }
        if mode == "proportional"
            && (int(&fresh, "unmatched_count") != 0 || gap.is_some())
            && !flag(data, "accept_difference")
        {
            return Err(Error::invalid("请确认未匹配费用按已匹配扣费比例分摊"));
        }
        if int(&fresh, "outside_count") != 0 && !flag(data, "accept_outside") {
            return Err(Error::invalid("请确认不计入周期范围外的账单记录"));
        }
        let local_only = fresh["manual_basis"]["source"] == "local_only";
        let bill = if local_only {
            json!({"id":null})
        } else {
            exec(db,"INSERT INTO bill_imports(cycle_id,file_name,file_hash,rows,created_by,created_at) VALUES(?,?,?,?,?,?) ON CONFLICT(cycle_id,file_hash) DO NOTHING",&[json!(cid),result["file_name"].clone(),result["file_hash"].clone(),json!(dumps(&payload["rows"])),actor["id"].clone(),json!(time::now())]).await?;
            one(
                db,
                "SELECT id FROM bill_imports WHERE cycle_id=? AND file_hash=?",
                &[json!(cid), result["file_hash"].clone()],
            )
            .await?
        };
        let basis_id = Self::save_manual_basis(
            db,
            actor,
            Some(cid),
            None,
            &merge(
                fresh.clone(),
                json!({"start":c["start"],"end":result["end"],"version":result["version"]}),
            ),
        )
        .await?;
        Self::clear_manual_draft(db, Some(cid), None).await?;
        exec(
            db,
            "UPDATE cycles SET manual_basis_id=? WHERE id=?",
            &[basis_id, json!(cid)],
        )
        .await?;
        exec(db,"UPDATE cycles SET cost_fen=?,cost_kind='exact',cost_source=?,bill_import_id=?,bill_end=?,bill_digest=?,bill_difference_confirmed=?,bill_allocation_mode=?,billable_cost_fen=?,version=version+1 WHERE id=? AND version=?",&[result["cost_fen"].clone(),json!(if local_only{"manual_bill"}else{"upstream_bill"}),bill["id"].clone(),result["end"].clone(),result["digest"].clone(),json!(i64::from(flag(data,"accept_difference"))),json!(mode),fresh["billable_cost_fen"].clone(),json!(cid),c["version"].clone()]).await?;
        audit(db,actor,"bill_allocation",json!(cid),&json!({"allocation_mode":mode,"billable_cost_fen":fresh["billable_cost_fen"],"excluded_amount":fresh["excluded_amount"],"rules_version":fresh["rules_version"]})).await?;
        audit(db,actor,if local_only{"confirm_local_billing"}else{"import_bill"},json!(cid),&json!({"before_cost_fen":c["cost_fen"],"cost_fen":result["cost_fen"],"bill_import_id":bill["id"],"file_hash":result["file_hash"],"end":result["end"],"unmatched_amount":result["unmatched_amount"],"accept_difference":flag(data,"accept_difference"),"accept_outside":flag(data,"accept_outside")})).await?;
        Ok(json!({"id":cid,"version":int(&c,"version")+1,"bill_import_id":bill["id"]}))
    }
}
