use super::*;

impl Service {
    pub(super) async fn manual_config(db: &mut Db, entity: &Value) -> Result<Option<Value>> {
        if entity["manual_basis_id"].is_null() {
            return Ok(None);
        }
        let row = one(
            db,
            "SELECT config FROM manual_bill_bases WHERE id=?",
            &[entity["manual_basis_id"].clone()],
        )
        .await?;
        Ok(Some(serde_json::from_str(text(&row, "config"))?))
    }
    pub(super) async fn save_manual_basis(
        db: &mut Db,
        actor: &Value,
        cycle: Option<i64>,
        period: Option<i64>,
        result: &Value,
    ) -> Result<Value> {
        if result["manual_basis"].is_null() {
            return Ok(Value::Null);
        }
        let id = store::exec(db,"INSERT INTO manual_bill_bases(cycle_id,vendor_period_id,config,result,created_by,created_at) VALUES(?,?,?,?,?,?)",&[json!(cycle),json!(period),json!(dumps(&result["manual_basis"])),json!(dumps(result)),actor["id"].clone(),json!(time::now())]).await?.last_insert_rowid();
        audit(db,actor,"confirm_manual_basis",json!(id),&json!({"cycle_id":cycle,"vendor_period_id":period,"basis":result["manual_basis"],"amount":result["manual_summary"]["amount"],"digest":result["digest"]})).await?;
        Ok(json!(id))
    }
}
