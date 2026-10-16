use super::*;
use crate::{
    json::{array, flag},
    money::Decimal,
    source::{Scope, Snapshot},
    store::{all, exec, optional},
};
use std::{collections::BTreeSet, time::Duration};
mod schedule;
use schedule::Checkpoint;
impl Service {
    pub async fn sync(&self, force: bool) -> Result<()> {
        let mut guard = self.sync_lock.lock().await;
        if !force && guard.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return Ok(());
        }
        *guard = Some(Instant::now());
        self.run_sync(None, !force).await
    }
    pub async fn sync_selection(
        &self,
        ids: &[Value],
        end: &str,
        start: Option<&str>,
    ) -> Result<()> {
        let mut _guard = self.sync_lock.lock().await;
        self.inventory.refresh(true).await;
        let inventory = self.inventory.state.read().await.rows.clone();
        let mut db = self.store.read().await?;
        let mut scopes = Vec::new();
        for id in ids {
            let c = one(
                &mut db,
                "SELECT * FROM cycles WHERE id=?",
                std::slice::from_ref(id),
            )
            .await?;
            let root = int(&c, "account_id");
            let lower = start.unwrap_or(text(&c, "start"));
            let actual_end = c["end"].as_str().unwrap_or(end);
            let accounts = inventory
                .values()
                .filter(|a| int(a, "root_id") == root)
                .map(|a| int(a, "id"))
                .collect();
            let known_ids = sqlx::query_scalar::<_,i64>("SELECT id FROM usage WHERE root_id=? AND created_at>=? AND created_at<? ORDER BY id")
                .bind(root).bind(lower).bind(actual_end).fetch_all(&mut *db).await?;
            scopes.push(Scope {
                root,
                accounts,
                start: lower.to_owned(),
                end: actual_end.to_owned(),
                known_ids,
            });
        }
        drop(db);
        self.run_sync(Some(scopes), false).await
    }
    async fn run_sync(&self, scopes: Option<Vec<Scope>>, incremental: bool) -> Result<()> {
        let started = Instant::now();
        let result = self.run_sync_inner(scopes, incremental).await;
        if result.is_err() {
            let mut db = self.store.write().await?;
            store::put(
                &mut db,
                "sync_error",
                &json!("消费记录同步失败，请稍后重试"),
            )
            .await?;
            db.commit().await?;
            tracing::warn!(
                task = "usage_sync",
                elapsed_ms = started.elapsed().as_millis() as u64,
                status = "failed"
            );
        } else {
            tracing::info!(
                task = "usage_sync",
                elapsed_ms = started.elapsed().as_millis() as u64,
                status = "complete"
            );
        }
        result
    }
    async fn run_sync_inner(
        &self,
        mut scopes: Option<Vec<Scope>>,
        incremental: bool,
    ) -> Result<()> {
        self.inventory.refresh(true).await;
        let mut checkpoint = Checkpoint::load(&self.store, scopes.is_none(), incremental).await?;
        let mut snapshot = if checkpoint.incremental() {
            self.source
                .incremental(checkpoint.watermark, &checkpoint.since()?)
                .await?
        } else {
            self.source.snapshot(scopes.clone()).await?
        };
        if scopes.is_some() || checkpoint.incremental() || checkpoint.daily() {
            let mut db = self.store.read().await?;
            let counts = store::meta(&mut db, "cleanup_counts").await?;
            let changed = snapshot.header.cleanups.iter().any(|t| {
                counts[int(t, "id").to_string()].as_i64().unwrap_or(0) != int(t, "deleted_rows")
            });
            drop(db);
            if changed {
                scopes = None;
                checkpoint.full();
                drop(snapshot);
                snapshot = self.source.snapshot(None).await?;
            }
        }
        let current: BTreeMap<_, _> = snapshot
            .header
            .accounts
            .iter()
            .cloned()
            .map(|a| (int(&a, "id"), a))
            .collect();
        let mut db = self.store.read().await?;
        let known: BTreeMap<_, _> = all(&mut db, "SELECT id,root_id FROM accounts", &[])
            .await?
            .iter()
            .map(|a| (int(a, "id"), int(a, "root_id")))
            .collect();
        drop(db);
        let roots = crate::inventory::roots(&current, &known)?;
        if (checkpoint.incremental() || checkpoint.daily())
            && checkpoint.roots_changed(&self.store, &roots).await?
        {
            checkpoint.full();
            snapshot.unrestrict().await?;
        }
        if checkpoint.daily() {
            checkpoint.fingerprints = Some(snapshot.fingerprints().await?);
            scopes = Some(
                checkpoint
                    .changed_scopes(&self.store, &roots, &known)
                    .await?,
            );
            snapshot
                .restrict(scopes.clone().unwrap_or_default())
                .await?;
        } else if checkpoint.incremental() {
            // An incremental cursor is not evidence that older records vanished.
            scopes = Some(vec![]);
        }
        {
            let mut db = self.store.write().await?;
            exec(&mut db, "DELETE FROM rust_sync_stage", &[]).await?;
            exec(&mut db, "DELETE FROM rust_sync_changed", &[]).await?;
            db.commit().await?;
        }
        self.stage(&mut snapshot, &roots, &known).await?;
        if checkpoint.reconcile() && checkpoint.fingerprints.is_none() {
            checkpoint.fingerprints = Some(snapshot.fingerprints().await?);
        }
        let mut db = self.store.write().await?;
        self.inventory.persist(&mut db).await?;
        for u in &snapshot.header.users {
            exec(&mut db,"INSERT INTO users VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,role=excluded.role,status=excluded.status,deleted=excluded.deleted",&[u["id"].clone(),u["name"].clone(),u["role"].clone(),u["status"].clone(),json!(i64::from(flag(u,"deleted")))]).await?;
        }
        if store::meta(&mut db, "coverage_from").await?.is_null() {
            let minimum = one(
                &mut db,
                "SELECT min(created_at) AS first FROM rust_sync_stage",
                &[],
            )
            .await?;
            store::put(
                &mut db,
                "coverage_from",
                &json!(minimum["first"].as_str().unwrap_or(&time::now())),
            )
            .await?;
        }
        self.coverage(
            &mut db,
            &snapshot.header.cleanups,
            &roots,
            &known,
            scopes.as_deref(),
        )
        .await?;
        let differs = "u.id IS NULL OR u.account_id!=s.account_id OR u.root_id!=s.root_id OR u.user_id!=s.user_id OR u.amount!=s.amount OR u.created_at!=s.created_at OR (s.detail IS NOT NULL AND (d.payload IS NULL OR d.payload!=s.detail))";
        exec(&mut db,&format!("INSERT OR IGNORE INTO rust_sync_changed SELECT s.root_id FROM rust_sync_stage s LEFT JOIN usage u ON u.id=s.id LEFT JOIN usage_details d ON d.usage_id=s.id WHERE {differs}"),&[]).await?;
        exec(&mut db,&format!("INSERT OR IGNORE INTO rust_sync_changed SELECT u.root_id FROM rust_sync_stage s JOIN usage u ON u.id=s.id LEFT JOIN usage_details d ON d.usage_id=s.id WHERE {differs}"),&[]).await?;
        exec(&mut db,"INSERT INTO usage SELECT s.id,s.account_id,s.root_id,s.user_id,s.amount,s.created_at FROM rust_sync_stage s LEFT JOIN usage u ON u.id=s.id WHERE u.id IS NULL OR u.account_id!=s.account_id OR u.root_id!=s.root_id OR u.user_id!=s.user_id OR u.amount!=s.amount OR u.created_at!=s.created_at ON CONFLICT(id) DO UPDATE SET account_id=excluded.account_id,root_id=excluded.root_id,user_id=excluded.user_id,amount=excluded.amount,created_at=excluded.created_at",&[]).await?;
        exec(&mut db,"INSERT INTO usage_details SELECT s.id,s.detail FROM rust_sync_stage s LEFT JOIN usage_details d ON d.usage_id=s.id WHERE s.detail IS NOT NULL AND (d.payload IS NULL OR d.payload!=s.detail) ON CONFLICT(usage_id) DO UPDATE SET payload=excluded.payload",&[]).await?;
        exec(&mut db,"INSERT INTO rust_root_versions(root_id,version) SELECT root_id,1 FROM rust_sync_changed WHERE 1 ON CONFLICT(root_id) DO UPDATE SET version=version+1",&[]).await?;
        store::put(&mut db, "last_sync", &json!(time::now())).await?;
        store::put(&mut db, "sync_error", &Value::Null).await?;
        let count = one(
            &mut db,
            "SELECT count(*) AS count FROM rust_source_presence",
            &[],
        )
        .await?;
        store::put(&mut db, "source_count", &count["count"]).await?;
        let changed = all(&mut db, "SELECT root_id FROM rust_sync_changed", &[])
            .await?
            .iter()
            .map(|r| int(r, "root_id"))
            .collect::<BTreeSet<_>>();
        let gaps = store::meta(&mut db, "coverage_gaps").await?;
        checkpoint.publish(&mut db, &roots).await?;
        db.commit().await?;
        let mut db = self.store.read().await?;
        let settlements = all(
            &mut db,
            "SELECT * FROM settlements WHERE status='active' AND cycle_id IS NOT NULL AND drift=0",
            &[],
        )
        .await?;
        let mut drift = Vec::new();
        for s in settlements {
            let snap: Value = serde_json::from_str(text(&s, "snapshot"))?;
            if changed.contains(&int(&snap["cycle"], "account_id")) || !array(&gaps).is_empty() {
                let calc = self
                    .preview_calculation(&mut db, &snap["cycle"], text(&snap, "end"))
                    .await?;
                if calc["digest"] != s["digest"]
                    || (snap["cycle"]["cost_source"] != "upstream_bill"
                        && Self::coverage_error(&mut db, &snap["cycle"], text(&snap, "end"))
                            .await?
                            .is_some())
                {
                    drift.push(s["id"].clone());
                }
            }
        }
        drop(db);
        if !drift.is_empty() {
            let mut db = self.store.write().await?;
            for id in drift {
                exec(&mut db, "UPDATE settlements SET drift=1 WHERE id=?", &[id]).await?;
            }
            db.commit().await?;
        }
        Ok(())
    }
    async fn stage(
        &self,
        snapshot: &mut Snapshot,
        roots: &BTreeMap<i64, i64>,
        known: &BTreeMap<i64, i64>,
    ) -> Result<()> {
        while let Some(batch) = snapshot.next().await? {
            let mut normalized = Vec::with_capacity(batch.len());
            for r in batch {
                let aid = int(&r, "account_id");
                let root = *roots.get(&aid).or(known.get(&aid)).unwrap_or(&aid);
                let mut detail = serde_json::Map::new();
                for k in crate::billing::DETAIL_FIELDS {
                    detail.insert(k.to_owned(), r[k].clone());
                }
                let detail = if detail.values().any(|v| !v.is_null()) {
                    Some(dumps(&Value::Object(detail)))
                } else {
                    None
                };
                normalized.push((
                    int(&r, "id"),
                    aid,
                    root,
                    int(&r, "user_id"),
                    Decimal::parse(text(&r, "amount"))?.to_string(),
                    time::instant(text(&r, "created_at"))?,
                    detail,
                ));
            }
            let mut db = self.store.write().await?;
            for chunk in normalized.chunks(256) {
                let mut q = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                    "INSERT INTO rust_sync_stage(id,account_id,root_id,user_id,amount,created_at,detail) ",
                );
                q.push_values(chunk, |mut b, r| {
                    b.push_bind(r.0)
                        .push_bind(r.1)
                        .push_bind(r.2)
                        .push_bind(r.3)
                        .push_bind(&r.4)
                        .push_bind(&r.5)
                        .push_bind(&r.6);
                });
                q.build().execute(&mut *db).await?;
            }
            db.commit().await?;
        }
        Ok(())
    }
    async fn coverage(
        &self,
        db: &mut Db,
        cleanups: &[Value],
        roots: &BTreeMap<i64, i64>,
        known: &BTreeMap<i64, i64>,
        scopes: Option<&[Scope]>,
    ) -> Result<()> {
        let mut presence_changed = false;
        if optional(
            db,
            "SELECT 1 AS found FROM rust_source_presence LIMIT 1",
            &[],
        )
        .await?
        .is_none()
        {
            let previous = store::meta(db, "source_ids").await?;
            for chunk in array(&previous).chunks(1024) {
                let mut q = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                    "INSERT OR IGNORE INTO rust_source_presence(id) ",
                );
                q.push_values(chunk, |mut b, id| {
                    b.push_bind(id.as_i64().unwrap_or(0));
                });
                presence_changed |= q.build().execute(&mut *db).await?.rows_affected() != 0;
            }
        }
        let mut pending: BTreeSet<i64> = array(&store::meta(db, "archived_deletions").await?)
            .iter()
            .filter_map(Value::as_i64)
            .collect();
        if let Some(scopes) = scopes {
            for scope in scopes {
                for chunk in scope.known_ids.chunks(512) {
                    let ids = json!(chunk);
                    let gone=all(db,"SELECT p.id FROM rust_source_presence p JOIN json_each(?) j ON j.value=p.id LEFT JOIN rust_sync_stage s ON s.id=p.id WHERE s.id IS NULL",&[json!(dumps(&ids))]).await?;
                    pending.extend(gone.iter().map(|r| int(r, "id")));
                    presence_changed |= exec(db,"DELETE FROM rust_source_presence WHERE id IN (SELECT value FROM json_each(?)) AND id NOT IN (SELECT id FROM rust_sync_stage)",&[json!(dumps(&ids))]).await?.rows_affected() != 0;
                }
            }
        } else {
            pending.extend(all(db,"SELECT p.id FROM rust_source_presence p LEFT JOIN rust_sync_stage s ON s.id=p.id WHERE s.id IS NULL",&[]).await?.iter().map(|r|int(r,"id")));
            presence_changed |= exec(
                db,
                "DELETE FROM rust_source_presence WHERE id NOT IN (SELECT id FROM rust_sync_stage)",
                &[],
            )
            .await?
            .rows_affected()
                != 0;
        }
        presence_changed |= exec(
            db,
            "INSERT OR IGNORE INTO rust_source_presence SELECT id FROM rust_sync_stage",
            &[],
        )
        .await?
        .rows_affected()
            != 0;
        let present=all(db,"SELECT value AS id FROM json_each(?) WHERE value IN (SELECT id FROM rust_source_presence)",&[json!(dumps(&json!(pending)))]).await?;
        for p in present {
            pending.remove(&int(&p, "id"));
        }
        let mut counts = store::meta(db, "cleanup_counts").await?;
        if counts.is_null() {
            counts = json!({});
        }
        let mut gaps = array(&store::meta(db, "coverage_gaps").await?).to_vec();
        for task in cleanups {
            let key = int(task, "id").to_string();
            let count = int(task, "deleted_rows");
            let delta = count - counts[&key].as_i64().unwrap_or(0);
            if delta <= 0 {
                continue;
            }
            let f = &task["filters"];
            let interval = (|| -> Result<(String, String)> {
                let start = time::instant(text(f, "start_time"))?;
                let end = time::format(
                    time::parse(text(f, "end_time"))?
                        .checked_add_signed(chrono::Duration::microseconds(1))
                        .ok_or_else(Error::unavailable)?,
                );
                if end <= start {
                    return Err(Error::invalid("时间范围无效"));
                }
                Ok((start, end))
            })();
            let valid = interval.is_ok();
            let (start, end) = match interval {
                Ok(p) => p,
                Err(_) => (
                    store::meta(db, "coverage_from")
                        .await?
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                    if task["finished_at"].is_null() {
                        time::now()
                    } else {
                        time::instant(text(task, "finished_at"))?
                    },
                ),
            };
            let precise = valid
                && f.as_object().is_none_or(|o| {
                    !o.iter().any(|(k, v)| {
                        !v.is_null()
                            && !["start_time", "end_time", "account_id", "user_id"]
                                .contains(&k.as_str())
                    })
                });
            let matches=all(db,"SELECT u.id FROM usage u JOIN json_each(?) j ON u.id=j.value WHERE u.created_at>=? AND u.created_at<? AND (? IS NULL OR u.account_id=?) AND (? IS NULL OR u.user_id=?) ORDER BY u.id",&[json!(dumps(&json!(pending))),json!(start),json!(end),f["account_id"].clone(),f["account_id"].clone(),f["user_id"].clone(),f["user_id"].clone()]).await?;
            let take = if !precise || matches.len() < (delta as usize) {
                let aid = int(f, "account_id");
                let gap = json!({"id":task["id"],"start":start,"end":end,"root_id":if f["account_id"].is_null(){None}else{Some(*roots.get(&aid).or(known.get(&aid)).unwrap_or(&aid))}});
                if let Some(prior) = gaps.iter_mut().find(|g| g["id"] == task["id"]) {
                    *prior = gap;
                } else {
                    gaps.push(gap);
                }
                matches.len()
            } else {
                delta as usize
            };
            for m in matches.iter().take(take) {
                pending.remove(&int(m, "id"));
            }
            counts[&key] = json!(count);
        }
        if presence_changed
            || optional(
                db,
                "SELECT 1 AS found FROM meta WHERE key='source_ids'",
                &[],
            )
            .await?
            .is_none()
        {
            let source_ids=one(db,"SELECT json_group_array(id) AS ids FROM (SELECT id FROM rust_source_presence ORDER BY id)",&[]).await?;
            exec(db,"INSERT INTO meta(key,value) VALUES('source_ids',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",&[source_ids["ids"].clone()]).await?;
        }
        store::put(db, "archived_deletions", &json!(pending)).await?;
        store::put(db, "cleanup_counts", &counts).await?;
        store::put(db, "coverage_gaps", &json!(gaps)).await?;
        Ok(())
    }
}
