use super::*;

impl Service {
    pub(super) async fn has_manual_draft(
        db: &mut Db,
        cycle: Option<i64>,
        period: Option<i64>,
    ) -> Result<bool> {
        Ok(store::optional(
            db,
            "SELECT 1 FROM manual_bill_drafts WHERE cycle_id=? OR vendor_period_id=?",
            &[json!(cycle), json!(period)],
        )
        .await?
        .is_some())
    }
    pub(super) async fn clear_manual_draft(
        db: &mut Db,
        cycle: Option<i64>,
        period: Option<i64>,
    ) -> Result<()> {
        store::exec(
            db,
            "DELETE FROM manual_bill_drafts WHERE cycle_id=? OR vendor_period_id=?",
            &[json!(cycle), json!(period)],
        )
        .await?;
        Ok(())
    }
    pub(super) async fn mark_manual_draft(
        db: &mut Db,
        cycle: Option<i64>,
        period: Option<i64>,
    ) -> Result<()> {
        Self::clear_manual_draft(db, cycle, period).await?;
        store::exec(
            db,
            "INSERT INTO manual_bill_drafts(cycle_id,vendor_period_id,created_at) VALUES(?,?,?)",
            &[json!(cycle), json!(period), json!(time::now())],
        )
        .await?;
        Ok(())
    }
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
        store::exec(db,"INSERT INTO manual_bill_bases(cycle_id,vendor_period_id,config,result,created_by,created_at) VALUES(?,?,?,?,?,?)",&[json!(cycle),json!(period),json!(dumps(&result["manual_basis"])),json!(dumps(result)),actor["id"].clone(),json!(time::now())]).await?;
        let row = one(db, "SELECT last_insert_rowid() AS id", &[]).await?;
        audit(db,actor,"confirm_manual_basis",row["id"].clone(),&json!({"cycle_id":cycle,"vendor_period_id":period,"basis":result["manual_basis"],"amount":result["manual_summary"]["amount"],"digest":result["digest"]})).await?;
        Ok(row["id"].clone())
    }
}
