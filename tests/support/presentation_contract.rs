use std::collections::{BTreeMap, HashMap, HashSet};

use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, Zero};
use serde_json::{Value, json};
use settlement::money::Decimal;

use super::{TestResult, canonical_json};

#[derive(Clone)]
struct CycleRef {
    id: i64,
    account_id: i64,
    fixture_name: String,
}

#[derive(Default)]
pub(super) struct PresentationContract {
    account_names: BTreeMap<i64, String>,
    user_roles: BTreeMap<i64, String>,
    previews: HashMap<String, Vec<CycleRef>>,
    new_settlement_names: BTreeMap<i64, String>,
    new_settlement_fixture_names: BTreeMap<i64, String>,
    new_settled_cycle_names: BTreeMap<i64, String>,
    new_order_names: BTreeMap<i64, String>,
    new_order_fixture_names: BTreeMap<i64, String>,
    settled_cycle_ids: HashSet<i64>,
}

impl PresentationContract {
    pub(super) fn set_source_state(&mut self, state: &Value) {
        let source = state.get("source").unwrap_or(state);
        self.account_names = source["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|account| {
                Some((
                    account["id"].as_i64()?,
                    account["name"].as_str()?.to_owned(),
                ))
            })
            .collect();
        self.user_roles = source["users"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|user| Some((user["id"].as_i64()?, user["role"].as_str()?.to_owned())))
            .collect();
    }

    pub(super) fn set_settled_cycle_ids(&mut self, ids: HashSet<i64>) {
        self.settled_cycle_ids = ids;
    }

    pub(super) fn record_preview(&mut self, step: &Value) -> TestResult<()> {
        if step["request"]["method"] != "POST"
            || step["request"]["path"] != "/settlement/api/v1/previews"
            || step["expect"]["status"] != 200
        {
            return Ok(());
        }
        let Some(preview_alias) = step["expect"]["body"]["id"].as_str() else {
            return Ok(());
        };
        if !preview_alias.starts_with("<preview:") {
            return Ok(());
        }
        let cycles = step["expect"]["body"]["cycles"]
            .as_array()
            .ok_or_else(|| format!("{}: preview fixture cycles are missing", step["label"]))?;
        let mut refs = Vec::new();
        for calculation in cycles {
            let Some(cycle) = calculation.get("cycle") else {
                continue;
            };
            let id = cycle["id"]
                .as_i64()
                .ok_or_else(|| format!("{}: preview fixture cycle id is missing", step["label"]))?;
            let account_id = cycle["account_id"].as_i64().ok_or_else(|| {
                format!("{}: preview fixture account id is missing", step["label"])
            })?;
            let fixture_name = cycle["name"].as_str().ok_or_else(|| {
                format!("{}: preview fixture cycle name is missing", step["label"])
            })?;
            refs.push(CycleRef {
                id,
                account_id,
                fixture_name: fixture_name.to_owned(),
            });
        }
        self.previews.insert(preview_alias.to_owned(), refs);
        Ok(())
    }

    pub(super) fn record_settlement(
        &mut self,
        step: &Value,
        request: &Value,
        response_body: &Value,
        response_status: u16,
        prior_settlement_ids: &HashSet<i64>,
    ) -> TestResult<()> {
        if response_status != 200
            || request["method"] != "POST"
            || request["path"] != "/settlement/api/v1/settlements"
        {
            return Ok(());
        }
        let Some(preview_alias) = request["json"]["preview_id"].as_str() else {
            return Ok(());
        };
        let refs = self
            .previews
            .get(preview_alias)
            .cloned()
            .unwrap_or_default();
        let Some(settlements) = response_body["settlements"].as_array() else {
            return Ok(());
        };
        for settlement in settlements {
            let Some(settlement_id) = settlement["settlement_id"].as_i64() else {
                continue;
            };
            if prior_settlement_ids.contains(&settlement_id)
                || self.new_settlement_names.contains_key(&settlement_id)
            {
                continue;
            }
            let Some(cycle_id) = settlement["cycle_id"].as_i64() else {
                continue;
            };
            let cycle = refs.iter().find(|cycle| cycle.id == cycle_id).ok_or_else(|| {
                format!(
                    "{}: new settlement {settlement_id} has no matching fixture preview cycle {cycle_id}",
                    step["label"].as_str().unwrap_or("settlement")
                )
            })?;
            let source_name = self
                .account_names
                .get(&cycle.account_id)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "{}: simulated source has no name for account {}",
                        step["label"].as_str().unwrap_or("settlement"),
                        cycle.account_id
                    )
                })?;
            self.new_settlement_names
                .insert(settlement_id, source_name.clone());
            self.new_settlement_fixture_names
                .insert(settlement_id, cycle.fixture_name.clone());
            self.new_settled_cycle_names
                .insert(cycle_id, source_name.clone());
        }
        Ok(())
    }

    pub(super) fn record_order_ids(&mut self, rows: &[(i64, i64)]) {
        for (order_id, settlement_id) in rows {
            if let Some(name) = self.new_settlement_names.get(settlement_id) {
                self.new_order_names.insert(*order_id, name.clone());
            }
            if let Some(name) = self.new_settlement_fixture_names.get(settlement_id) {
                self.new_order_fixture_names.insert(*order_id, name.clone());
            }
        }
    }

    pub(super) fn restore_fixture_order_names(&self, rows: &mut [Value]) {
        for order in rows {
            if let Some(order_id) = order["id"].as_i64()
                && let Some(name) = self.new_order_fixture_names.get(&order_id)
            {
                order["account_name"] = json!(name);
            }
        }
    }

    pub(super) fn adapt_response_body(&self, step: &Value, body: &mut Value) -> TestResult<()> {
        let request = &step["request"];
        let method = request["method"].as_str().unwrap_or("");
        let path = request["path"].as_str().unwrap_or("");
        match (method, path) {
            ("GET", "/settlement/api/v1/accounts/state") => {
                self.adapt_account_state(body, step)?;
            }
            ("GET", "/settlement/api/v1/overview") => {
                self.adapt_overview(body, step)?;
            }
            ("POST", "/settlement/api/v1/previews") => {
                self.adapt_preview(body)?;
            }
            ("GET", "/settlement/api/v1/history") => {
                self.adapt_history(body);
            }
            ("GET", path) if path.starts_with("/settlement/api/v1/cycles/") => {
                self.adapt_cycle_detail(body);
            }
            ("GET", "/settlement/api/v1/orders") => {
                self.adapt_orders(body);
            }
            ("GET", path) if path.starts_with("/settlement/api/v1/orders/") => {
                self.adapt_order_detail(body);
            }
            ("GET", path) if path.starts_with("/settlement/api/v1/netting/balances/") => {
                self.adapt_orders(body);
            }
            ("GET", path) if path.starts_with("/settlement/api/v1/account-deletions") => {
                self.adapt_deletion_list(body);
            }
            ("POST", path) if path.starts_with("/settlement/api/v1/account-deletions/") => {
                self.adapt_deletion_list(body);
            }
            ("POST", "/settlement/api/v1/settlements") => {
                self.adapt_deletions(body);
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn adapt_database_snapshot(&self, expected: &mut Value) {
        let Some(tables) = expected["database"].as_object_mut() else {
            return;
        };
        if let Some(cycles) = tables.get_mut("cycles").and_then(Value::as_array_mut) {
            for cycle in cycles {
                let Some(id) = cycle["id"].as_i64() else {
                    continue;
                };
                if let Some(source_name) = self.new_settled_cycle_names.get(&id) {
                    cycle["name"] = json!(source_name);
                }
            }
        }
        if let Some(settlements) = tables.get_mut("settlements").and_then(Value::as_array_mut) {
            for settlement in settlements {
                let Some(id) = settlement["id"].as_i64() else {
                    continue;
                };
                if let Some(source_name) = self.new_settlement_names.get(&id)
                    && let Some(cycle) = settlement["snapshot"]["cycle"].as_object_mut()
                {
                    cycle.insert("name".to_owned(), json!(source_name));
                }
            }
        }
        if let Some(orders) = tables.get_mut("orders").and_then(Value::as_array_mut) {
            for order in orders {
                if let Some(settlement_id) = order["settlement_id"].as_i64()
                    && let Some(source_name) = self.new_settlement_names.get(&settlement_id)
                {
                    order["account_name"] = json!(source_name);
                }
            }
        }
        if let Some(deletions) = tables
            .get_mut("account_deletions")
            .and_then(Value::as_array_mut)
        {
            for deletion in deletions {
                if let Some(settlement_id) = deletion["settlement_id"].as_i64()
                    && let Some(source_name) = self.new_settlement_names.get(&settlement_id)
                {
                    deletion["account_name"] = json!(source_name);
                }
            }
        }
        if let Some(payments) = tables
            .get_mut("netting_payments")
            .and_then(Value::as_array_mut)
        {
            for payment in payments {
                if let Some(orders) = payment
                    .get_mut("snapshot")
                    .and_then(|snapshot| snapshot.get_mut("orders"))
                    .and_then(Value::as_array_mut)
                {
                    self.adapt_order_rows(orders);
                }
            }
        }
        for table in [
            "cycles",
            "settlements",
            "orders",
            "account_deletions",
            "netting_payments",
        ] {
            if let Some(rows) = tables.get_mut(table).and_then(Value::as_array_mut) {
                rows.sort_by_key(canonical_json);
            }
        }
    }

    fn adapt_account_state(&self, body: &mut Value, step: &Value) -> TestResult<()> {
        if body.get("accounts").is_some() {
            return Ok(());
        }
        let active_ids = body["active_ids"]
            .as_array()
            .ok_or_else(|| format!("{}: account-state active_ids are missing", step["label"]))?;
        let accounts = active_ids
            .iter()
            .map(|id| {
                let id = id.as_i64().ok_or_else(|| {
                    format!("{}: account-state id is not an integer", step["label"])
                })?;
                let name = self.account_names.get(&id).ok_or_else(|| {
                    format!(
                        "{}: simulated source has no name for active account {id}",
                        step["label"]
                    )
                })?;
                Ok(json!({"id":id,"name":name}))
            })
            .collect::<TestResult<Vec<_>>>()?;
        body["accounts"] = json!(accounts);
        Ok(())
    }

    fn adapt_overview(&self, body: &mut Value, step: &Value) -> TestResult<()> {
        let Some(accounts) = body.get_mut("accounts").and_then(Value::as_array_mut) else {
            return Ok(());
        };
        let actor = step["request"]["actor"]
            .as_i64()
            .ok_or_else(|| format!("{}: overview request actor is missing", step["label"]))?;
        let role = self.user_roles.get(&actor).ok_or_else(|| {
            format!(
                "{}: simulated source has no role for actor {actor}",
                step["label"]
            )
        })?;
        if role != "admin" {
            accounts.retain(|account| account["kind"] == "paid");
        }
        for account in accounts {
            if let Some(id) = account["id"].as_i64()
                && let Some(name) = self.account_names.get(&id)
            {
                account["name"] = json!(name);
            }
            if let Some(cycle) = account.get_mut("cycle").filter(|cycle| !cycle.is_null()) {
                self.adapt_cycle_row(cycle);
            }
        }
        Ok(())
    }

    fn adapt_preview(&self, body: &mut Value) -> TestResult<()> {
        let Some(calculations) = body.get_mut("cycles").and_then(Value::as_array_mut) else {
            return Ok(());
        };
        for calculation in calculations {
            if let Some(cycle) = calculation.get_mut("cycle")
                && let Some(account_id) = cycle["account_id"].as_i64()
                && let Some(name) = self.account_names.get(&account_id)
            {
                cycle["name"] = json!(name);
            }
            set_display(calculation, false);
        }
        Ok(())
    }

    fn adapt_history(&self, body: &mut Value) {
        if let Some(cycles) = body.get_mut("cycles").and_then(Value::as_array_mut) {
            for cycle in cycles {
                self.adapt_cycle_row(cycle);
            }
        }
    }

    fn adapt_cycle_detail(&self, body: &mut Value) {
        let Some(cycle) = body.get_mut("cycle") else {
            return;
        };
        self.adapt_cycle_detail_name(cycle);
        let settled = cycle["id"]
            .as_i64()
            .is_some_and(|id| self.settled_cycle_ids.contains(&id));
        set_display(body, settled);
    }

    fn adapt_orders(&self, body: &mut Value) {
        if let Some(orders) = body.get_mut("orders").and_then(Value::as_array_mut) {
            self.adapt_order_rows(orders);
        }
    }

    fn adapt_order_rows(&self, orders: &mut [Value]) {
        for order in orders {
            if let Some(settlement_id) = order["settlement_id"].as_i64()
                && let Some(source_name) = self.new_settlement_names.get(&settlement_id)
            {
                set_order_name(order, source_name);
            } else if let Some(order_id) = order["id"].as_i64()
                && let Some(source_name) = self.new_order_names.get(&order_id)
            {
                set_order_name(order, source_name);
            }
        }
    }

    fn adapt_deletions(&self, body: &mut Value) {
        if let Some(deletions) = body.get_mut("deletions").and_then(Value::as_array_mut) {
            for deletion in deletions {
                self.adapt_deletion_row(deletion);
            }
        }
    }

    fn adapt_deletion_list(&self, body: &mut Value) {
        if let Some(items) = body.get_mut("items").and_then(Value::as_array_mut) {
            for item in items {
                self.adapt_deletion_row(item);
            }
        }
        if let Some(deletion) = body.get_mut("deletion") {
            self.adapt_deletion_row(deletion);
        }
        if body.get("settlement_id").is_some() {
            self.adapt_deletion_row(body);
        }
    }

    fn adapt_deletion_row(&self, deletion: &mut Value) {
        if let Some(settlement_id) = deletion["settlement_id"].as_i64()
            && let Some(source_name) = self.new_settlement_names.get(&settlement_id)
        {
            deletion["account_name"] = json!(source_name);
        }
    }

    fn adapt_order_detail(&self, body: &mut Value) {
        let settlement_id = body["order"]["settlement_id"].as_i64();
        let source_name = settlement_id.and_then(|id| self.new_settlement_names.get(&id));
        if let Some(source_name) = source_name {
            set_order_name(&mut body["order"], source_name);
            if let Some(cycle) = body
                .get_mut("snapshot")
                .and_then(|snapshot| snapshot.get_mut("cycle"))
            {
                cycle["name"] = json!(source_name);
            }
        }
        if let Some(snapshot) = body.get_mut("snapshot") {
            set_display(snapshot, true);
        }
    }

    fn adapt_cycle_row(&self, cycle: &mut Value) {
        if cycle["status"] == "open" {
            if let Some(account_id) = cycle["account_id"].as_i64()
                && let Some(name) = self.account_names.get(&account_id)
            {
                cycle["name"] = json!(name);
            }
            return;
        }
        self.adapt_cycle_detail_name(cycle);
    }

    fn adapt_cycle_detail_name(&self, cycle: &mut Value) {
        if let Some(id) = cycle["id"].as_i64()
            && let Some(name) = self.new_settled_cycle_names.get(&id)
        {
            cycle["name"] = json!(name);
            return;
        }
        if cycle["status"] == "open"
            && let Some(account_id) = cycle["account_id"].as_i64()
            && let Some(name) = self.account_names.get(&account_id)
        {
            cycle["name"] = json!(name);
            return;
        }
        if let Some(settlement_id) = cycle["settlement_id"].as_i64()
            && let Some(name) = self.new_settlement_names.get(&settlement_id)
        {
            cycle["name"] = json!(name);
        }
    }
}

fn set_order_name(order: &mut Value, source_name: &str) {
    if order.get("account_name").is_some() {
        order["account_name"] = json!(source_name);
    }
    if order.get("source_name").is_some() {
        order["source_name"] = json!(source_name);
    }
}

fn set_display(value: &mut Value, settled: bool) {
    if !value.is_object() || value.get("cycle").is_none() {
        return;
    }
    let cost_fen = value["bill"]["billable_cost_fen"]
        .as_i64()
        .or_else(|| value["cycle"]["cost_fen"].as_i64());
    let consumption = value["total_consumption"].as_str();
    value["display"] = json!({
        "cost_multiplier": cost_fen.and_then(|cost| multiplier(cost, consumption)),
        "settled": settled,
    });
}

fn multiplier(cost_fen: i64, consumption: Option<&str>) -> Option<String> {
    let total = Decimal::parse(consumption?).ok()?;
    if cost_fen < 0 || !total.positive() {
        return None;
    }
    let scaled = BigRational::from_integer(BigInt::from(cost_fen) * 100) / total.rational();
    if scaled > BigRational::zero() && scaled < BigRational::one() {
        return Some("<0.0001".to_owned());
    }
    let rounded = (scaled + BigRational::new(BigInt::one(), BigInt::from(2)))
        .floor()
        .to_integer();
    let digits = format!("{:0>5}", rounded);
    let split = digits.len() - 4;
    Some(format!("{}.{}", &digits[..split], &digits[split..]))
}
