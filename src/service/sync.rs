use super::*;
use crate::{
    json::{array, flag},
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
            let mut known_ids = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM usage WHERE root_id=? AND created_at>=? AND created_at<?",
            )
            .bind(root)
            .bind(lower)
            .bind(actual_end)
            .fetch_all(&mut *db)
            .await?;
            known_ids.sort_unstable();
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
        if scopes.is_none() {
            self.inventory.refresh(true).await;
        }
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
        let cleared = self.stage(&mut snapshot, &roots, &known).await?;
        if checkpoint.reconcile() && checkpoint.fingerprints.is_none() {
            checkpoint.fingerprints = Some(snapshot.fingerprints().await?);
        }
        let mut db = self.store.write().await?;
        if !cleared {
            exec(&mut db, "DELETE FROM rust_sync_stage", &[]).await?;
            exec(&mut db, "DELETE FROM rust_sync_changed", &[]).await?;
        }
        let persisted_seq = if self.inventory.needs_persist().await {
            Some(self.inventory.persist_seq(&mut db).await?)
        } else {
            None
        };
        for chunk in snapshot.header.users.chunks(256) {
            let mut q = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                "INSERT INTO users(id,name,role,status,deleted) ",
            );
            q.push_values(chunk, |mut b, u| {
                b.push_bind(int(u, "id"))
                    .push_bind(text(u, "name"))
                    .push_bind(text(u, "role"))
                    .push_bind(text(u, "status"))
                    .push_bind(i64::from(flag(u, "deleted")));
            });
            q.push(
                " ON CONFLICT(id) DO UPDATE SET name=excluded.name,role=excluded.role,status=excluded.status,deleted=excluded.deleted WHERE users.name IS NOT excluded.name OR users.role IS NOT excluded.role OR users.status IS NOT excluded.status OR users.deleted IS NOT excluded.deleted",
            );
            q.build().execute(&mut *db).await?;
        }
        let (old_coverage_from, old_gaps) = Self::coverage_meta(&mut db).await?;
        if old_coverage_from.is_null() {
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
        let mut changed_windows = BTreeMap::<i64, (String, String)>::new();
        {
            use futures_util::TryStreamExt;
            use sqlx::Row;
            let mut stream = sqlx::query(
                "SELECT s.root_id, s.created_at, u.root_id, u.created_at FROM rust_sync_stage s LEFT JOIN usage u ON u.id=s.id LEFT JOIN usage_details d ON d.usage_id=s.id WHERE u.id IS NULL OR u.account_id!=s.account_id OR u.root_id!=s.root_id OR u.user_id!=s.user_id OR u.amount!=s.amount OR u.created_at!=s.created_at OR (s.detail IS NOT NULL AND (d.payload IS NULL OR d.payload!=s.detail))",
            )
            .fetch(&mut *db);
            let mut record_window = |root_id: i64, created_at: &str| {
                changed_windows
                    .entry(root_id)
                    .and_modify(|(min_at, max_at)| {
                        if created_at < min_at.as_str() {
                            min_at.clear();
                            min_at.push_str(created_at);
                        }
                        if created_at > max_at.as_str() {
                            max_at.clear();
                            max_at.push_str(created_at);
                        }
                    })
                    .or_insert_with(|| (created_at.to_owned(), created_at.to_owned()));
            };
            while let Some(row) = stream.try_next().await? {
                let s_root: i64 = row.try_get(0)?;
                let s_at: &str = row.try_get(1)?;
                record_window(s_root, s_at);
                if let (Some(u_root), Some(u_at)) = (
                    row.try_get::<Option<i64>, _>(2)?,
                    row.try_get::<Option<&str>, _>(3)?,
                ) {
                    record_window(u_root, u_at);
                }
            }
        }
        if !changed_windows.is_empty() {
            let root_ids: Vec<i64> = changed_windows.keys().copied().collect();
            for chunk in root_ids.chunks(1024) {
                let mut q = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                    "INSERT OR IGNORE INTO rust_sync_changed(root_id) ",
                );
                q.push_values(chunk, |mut b, id| {
                    b.push_bind(*id);
                });
                q.build().execute(&mut *db).await?;
            }
            exec(&mut db,"INSERT INTO usage SELECT s.id,s.account_id,s.root_id,s.user_id,s.amount,s.created_at FROM rust_sync_stage s LEFT JOIN usage u ON u.id=s.id WHERE u.id IS NULL OR u.account_id!=s.account_id OR u.root_id!=s.root_id OR u.user_id!=s.user_id OR u.amount!=s.amount OR u.created_at!=s.created_at ON CONFLICT(id) DO UPDATE SET account_id=excluded.account_id,root_id=excluded.root_id,user_id=excluded.user_id,amount=excluded.amount,created_at=excluded.created_at",&[]).await?;
            exec(&mut db,"INSERT INTO usage_details SELECT s.id,s.detail FROM rust_sync_stage s LEFT JOIN usage_details d ON d.usage_id=s.id WHERE s.detail IS NOT NULL AND (d.payload IS NULL OR d.payload!=s.detail) ON CONFLICT(usage_id) DO UPDATE SET payload=excluded.payload",&[]).await?;
            exec(&mut db,"INSERT INTO rust_root_versions(root_id,version) SELECT root_id,1 FROM rust_sync_changed WHERE 1 ON CONFLICT(root_id) DO UPDATE SET version=version+1",&[]).await?;
        }
        store::put(&mut db, "last_sync", &json!(time::now())).await?;
        store::put(&mut db, "sync_error", &Value::Null).await?;
        let (coverage_from, gaps) = Self::coverage_meta(&mut db).await?;
        let coverage_changed = coverage_from != old_coverage_from || gaps != old_gaps;
        let check_coverage =
            coverage_changed || (checkpoint.reconcile() && !array(&gaps).is_empty());
        checkpoint.publish(&mut db, &roots).await?;
        exec(&mut db, "DELETE FROM rust_sync_stage", &[]).await?;
        exec(&mut db, "DELETE FROM rust_sync_changed", &[]).await?;
        if scopes.is_none() {
            exec(
                &mut db,
                "DELETE FROM sessions WHERE expires_at<=?",
                &[json!(time::epoch())],
            )
            .await?;
            let cutoff = json!(time::after(-3600));
            for table in ["previews", "bill_previews", "vendor_bill_previews"] {
                debug_assert!(matches!(
                    table,
                    "previews" | "bill_previews" | "vendor_bill_previews"
                ));
                exec(
                    &mut db,
                    &format!("DELETE FROM {table} WHERE expires_at<?"),
                    std::slice::from_ref(&cutoff),
                )
                .await?;
            }
        }
        db.commit().await?;
        if let Some(seq) = persisted_seq {
            self.inventory.mark_persisted(seq).await;
        }
        if changed_windows.is_empty() && !check_coverage {
            return Ok(());
        }
        let mut db = self.store.read().await?;
        let candidates = all(
            &mut db,
            "SELECT s.id,s.digest,c.id AS cycle_id,c.account_id,c.vendor_period_id,c.start,c.end,c.cost_source FROM settlements s JOIN cycles c ON c.id=s.cycle_id WHERE s.status='active' AND s.drift=0",
            &[],
        )
        .await?;
        let mut drift = Vec::new();
        let mut member_cache = BTreeMap::<i64, Vec<Value>>::new();
        let mut vendor_cache = BTreeMap::<i64, Value>::new();
        let overlaps = |aid: i64, start: &str, end: &str| -> bool {
            changed_windows
                .get(&aid)
                .is_some_and(|(min_at, max_at)| max_at.as_str() >= start && min_at.as_str() < end)
        };
        for m in candidates {
            let pid = int(&m, "vendor_period_id");
            let end = text(&m, "end");
            let affected = if pid != 0 {
                if let std::collections::btree_map::Entry::Vacant(entry) = member_cache.entry(pid) {
                    entry.insert(Self::vendor_members(&mut db, pid).await?);
                }
                member_cache[&pid].iter().any(|c| {
                    let aid = int(c, "account_id");
                    overlaps(aid, text(c, "start"), end)
                        || (check_coverage
                            && Self::coverage_error_from(&coverage_from, &gaps, c, end).is_some())
                })
            } else {
                let aid = int(&m, "account_id");
                let start = text(&m, "start");
                overlaps(aid, start, end)
                    || (check_coverage
                        && m["cost_source"] != "upstream_bill"
                        && Self::coverage_error_from(
                            &coverage_from,
                            &gaps,
                            &json!({"account_id":aid,"start":start}),
                            end,
                        )
                        .is_some())
            };
            if !affected {
                continue;
            }
            if pid != 0 {
                if let std::collections::btree_map::Entry::Vacant(entry) = vendor_cache.entry(pid) {
                    entry.insert(
                        self.vendor_calculation(&mut db, pid, end, None, false)
                            .await?,
                    );
                }
                let calc = array(&vendor_cache[&pid]["calculations"])
                    .iter()
                    .find(|c| c["cycle"]["id"] == m["cycle_id"])
                    .ok_or_else(Error::unavailable)?;
                if calc["digest"] != m["digest"] {
                    drift.push(m["id"].clone());
                }
            } else {
                let s = one(
                    &mut db,
                    "SELECT snapshot FROM settlements WHERE id=?",
                    std::slice::from_ref(&m["id"]),
                )
                .await?;
                let snap: Value = serde_json::from_str(text(&s, "snapshot"))?;
                let snap_end = text(&snap, "end");
                let calc = self
                    .preview_calculation(&mut db, &snap["cycle"], snap_end)
                    .await?;
                if calc["digest"] != m["digest"]
                    || (snap["cycle"]["cost_source"] != "upstream_bill"
                        && Self::coverage_error_from(
                            &coverage_from,
                            &gaps,
                            &snap["cycle"],
                            snap_end,
                        )
                        .is_some())
                {
                    drift.push(m["id"].clone());
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
    ) -> Result<bool> {
        let mut cleared = false;
        while let Some(batch) = snapshot.next().await? {
            let mut normalized = Vec::with_capacity(batch.len());
            for r in batch {
                let root = *roots
                    .get(&r.account_id)
                    .or(known.get(&r.account_id))
                    .unwrap_or(&r.account_id);
                let detail = (!r.detail.is_empty()).then(|| dumps(&r.detail));
                normalized.push((
                    r.id,
                    r.account_id,
                    root,
                    r.user_id,
                    r.amount,
                    r.created_at,
                    detail,
                ));
            }
            let mut db = self.store.write().await?;
            if !cleared {
                exec(&mut db, "DELETE FROM rust_sync_stage", &[]).await?;
                exec(&mut db, "DELETE FROM rust_sync_changed", &[]).await?;
                cleared = true;
            }
            for chunk in normalized.chunks(1024) {
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
        Ok(cleared)
    }
    async fn coverage(
        &self,
        db: &mut Db,
        cleanups: &[Value],
        roots: &BTreeMap<i64, i64>,
        known: &BTreeMap<i64, i64>,
        scopes: Option<&[Scope]>,
    ) -> Result<()> {
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
                q.build().execute(&mut *db).await?;
            }
        }
        let mut pending: BTreeSet<i64> = array(&store::meta(db, "archived_deletions").await?)
            .iter()
            .filter_map(Value::as_i64)
            .collect();
        if let Some(scopes) = scopes {
            for scope in scopes {
                for chunk in scope.known_ids.chunks(2048) {
                    let ids = dumps(&chunk);
                    let gone = sqlx::query_scalar::<_, i64>(
                        "SELECT p.id FROM rust_source_presence p JOIN json_each(?) j ON j.value=p.id LEFT JOIN rust_sync_stage s ON s.id=p.id WHERE s.id IS NULL",
                    )
                    .bind(&ids)
                    .fetch_all(&mut *db)
                    .await?;
                    if !gone.is_empty() {
                        pending.extend(gone.iter().copied());
                        exec(
                            db,
                            "DELETE FROM rust_source_presence WHERE id IN (SELECT value FROM json_each(?))",
                            &[Value::String(dumps(&gone))],
                        )
                        .await?;
                    }
                }
            }
        } else {
            pending.extend(all(db,"SELECT p.id FROM rust_source_presence p LEFT JOIN rust_sync_stage s ON s.id=p.id WHERE s.id IS NULL",&[]).await?.iter().map(|r|int(r,"id")));
            exec(
                db,
                "DELETE FROM rust_source_presence WHERE id NOT IN (SELECT id FROM rust_sync_stage)",
                &[],
            )
            .await?;
        }
        exec(
            db,
            "INSERT OR IGNORE INTO rust_source_presence SELECT id FROM rust_sync_stage",
            &[],
        )
        .await?;
        if !pending.is_empty() {
            let present = sqlx::query_scalar::<_, i64>(
                "SELECT value AS id FROM json_each(?) WHERE value IN (SELECT id FROM rust_source_presence)",
            )
            .bind(dumps(&pending))
            .fetch_all(&mut *db)
            .await?;
            for id in present {
                pending.remove(&id);
            }
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
                        .checked_add(time::Duration::microseconds(1))
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
        store::put(db, "archived_deletions", &json!(pending)).await?;
        store::put(db, "cleanup_counts", &counts).await?;
        store::put(db, "coverage_gaps", &json!(gaps)).await?;
        Ok(())
    }
}
