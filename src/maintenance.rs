//! Explicit offline maintenance; schema migrations never reset user configuration.
use crate::{
    error::{Error, Result},
    json::{digest, dumps, int},
    store::{self, Store},
    time,
};
use serde_json::{Value, json};
use sqlx::{
    SqliteConnection,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::path::Path;

async fn candidates(db: &mut SqliteConnection) -> Result<Vec<Value>> {
    store::all(db, "SELECT c.id,c.account_id,c.version,c.cost_fen,c.creditor_id,c.start,c.start_locked,p.version AS profile_version FROM cycles c JOIN profiles p ON p.account_id=c.account_id WHERE c.status='open' AND c.cost_kind='estimated' AND c.settlement_id IS NULL AND c.vendor_period_id IS NULL AND p.kind='paid' AND NOT EXISTS(SELECT 1 FROM settlements s WHERE s.cycle_id=c.id AND s.status='active') AND NOT EXISTS(SELECT 1 FROM account_deletions d WHERE d.account_id=c.account_id AND d.status IN ('queued','running','verify')) ORDER BY c.id", &[]).await
}

pub async fn preview(path: &Path) -> Result<Value> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false)
                .read_only(true),
        )
        .await?;
    let result = async {
        let mut db = pool.begin().await?;
        let rows = candidates(&mut db).await?;
        Ok(json!({"count":rows.len(),"cycle_ids":rows.iter().map(|c|c["id"].clone()).collect::<Vec<_>>(),"digest":digest(&json!(rows))}))
    }.await;
    pool.close().await;
    result
}

pub async fn reset_estimated(store: &Store, expected: &str) -> Result<Value> {
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::invalid("请提供重置预检摘要"));
    }
    let mut db = store.write().await?;
    let key = format!("reset_estimated:{expected}");
    let previous = store::meta(&mut db, &key).await?;
    if !previous.is_null() {
        return Ok(previous);
    }
    let rows = candidates(&mut db).await?;
    if digest(&json!(rows)) != expected {
        return Err(Error::conflict("费用配置已变化，请重新预检，未执行重置"));
    }
    let stamp = time::now();
    for row in &rows {
        let id = &row["id"];
        let affected=store::exec(&mut db,"UPDATE cycles SET status='cancelled',version=version+1 WHERE id=? AND version=? AND status='open' AND cost_kind='estimated' AND settlement_id IS NULL",&[id.clone(),row["version"].clone()]).await?.rows_affected();
        if affected != 1 {
            return Err(Error::conflict("费用配置已变化，请重新预检"));
        }
        store::exec(&mut db,"DELETE FROM profiles WHERE account_id=? AND kind='paid' AND version=? AND NOT EXISTS(SELECT 1 FROM cycles WHERE account_id=? AND status='open')",&[row["account_id"].clone(),row["profile_version"].clone(),row["account_id"].clone()]).await?;
        store::exec(
            &mut db,
            "DELETE FROM bill_previews WHERE cycle_id=?",
            std::slice::from_ref(id),
        )
        .await?;
        store::exec(&mut db,"DELETE FROM previews WHERE EXISTS(SELECT 1 FROM json_each(previews.payload) item WHERE json_extract(item.value,'$.cycle.id')=?)",std::slice::from_ref(id)).await?;
        store::exec(&mut db,"INSERT INTO audit(actor_id,action,entity_id,payload,created_at) VALUES(0,'reset_estimated',?,?,?)",&[id.clone(),json!(dumps(&json!({"before":row,"operation":"offline_maintenance","digest":expected}))),json!(stamp)]).await?;
    }
    let response = json!({"reset_count":rows.len(),"cycle_ids":rows.iter().map(|r|int(r,"id")).collect::<Vec<_>>(),"digest":expected});
    store::exec(
        &mut db,
        "INSERT INTO meta(key,value) VALUES(?,?)",
        &[json!(key), json!(dumps(&response))],
    )
    .await?;
    db.commit().await?;
    Ok(response)
}
