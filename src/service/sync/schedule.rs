use super::*;
use crate::source::Fingerprint;

pub(super) struct Checkpoint {
    enabled: bool,
    mode: &'static str,
    pub watermark: i64,
    last_incremental: String,
    last_reconciled: String,
    pub fingerprints: Option<Vec<Fingerprint>>,
}

impl Checkpoint {
    pub async fn load(store: &Store, enabled: bool, incremental: bool) -> Result<Self> {
        let mut db = store.read().await?;
        let previous = optional(&mut db, "SELECT * FROM rust_sync_state WHERE id=1", &[])
            .await?
            .unwrap_or(Value::Null);
        let last = text(&previous, "last_reconciled");
        let due = time::parse(last)
            .map(|at| {
                time::parse(&time::now())
                    .is_ok_and(|now| now.signed_duration_since(at).num_seconds() >= 86400)
            })
            .unwrap_or(true);
        Ok(Self {
            enabled,
            mode: if !enabled {
                "selected"
            } else if !incremental || previous.is_null() {
                "full"
            } else if due {
                "daily"
            } else {
                "incremental"
            },
            watermark: int(&previous, "watermark"),
            last_incremental: text(&previous, "last_incremental").to_owned(),
            last_reconciled: last.to_owned(),
            fingerprints: None,
        })
    }
    pub fn incremental(&self) -> bool {
        self.mode == "incremental"
    }
    pub fn daily(&self) -> bool {
        self.mode == "daily"
    }
    pub fn reconcile(&self) -> bool {
        self.enabled && !self.incremental()
    }
    pub fn full(&mut self) {
        if self.enabled {
            self.mode = "full";
        }
    }
    pub fn since(&self) -> Result<String> {
        Ok(time::format(
            time::parse(&self.last_incremental)? - chrono::Duration::minutes(10),
        ))
    }
    pub async fn roots_changed(&self, store: &Store, roots: &BTreeMap<i64, i64>) -> Result<bool> {
        let mut db = store.read().await?;
        let rows = all(
            &mut db,
            "SELECT account_id,root_id FROM rust_sync_roots",
            &[],
        )
        .await?;
        Ok(rows.iter().any(|row| {
            roots
                .get(&int(row, "account_id"))
                .is_some_and(|root| *root != int(row, "root_id"))
        }))
    }
    pub async fn changed_scopes(
        &self,
        store: &Store,
        roots: &BTreeMap<i64, i64>,
        known: &BTreeMap<i64, i64>,
    ) -> Result<Vec<Scope>> {
        let mut db = store.read().await?;
        let previous = all(&mut db, "SELECT * FROM rust_usage_fingerprints", &[]).await?;
        let old: BTreeMap<_, _> = previous
            .into_iter()
            .map(|r| ((int(&r, "account_id"), text(&r, "day").to_owned()), r))
            .collect();
        let current: BTreeMap<_, _> = self
            .fingerprints
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|f| ((f.account_id, f.day.clone()), f))
            .collect();
        let keys: BTreeSet<_> = old.keys().chain(current.keys()).cloned().collect();
        let mut scopes = Vec::new();
        for (account, day) in keys {
            let key = (account, day.clone());
            if let (Some(old), Some(new)) = (old.get(&key), current.get(&key))
                && int(old, "record_count") == new.count
                && text(old, "amount") == new.amount
                && text(old, "digest") == new.digest
                && roots.get(&account) == known.get(&account)
            {
                continue;
            }
            let start = time::instant(&format!("{day}T00:00:00Z"))?;
            let end = time::format(time::parse(&start)? + chrono::Duration::days(1));
            let known_ids = sqlx::query_scalar::<_,i64>("SELECT id FROM usage WHERE account_id=? AND created_at>=? AND created_at<? ORDER BY id")
                .bind(account).bind(&start).bind(&end).fetch_all(&mut *db).await?;
            scopes.push(Scope {
                root: *roots
                    .get(&account)
                    .or(known.get(&account))
                    .unwrap_or(&account),
                accounts: vec![account],
                start,
                end,
                known_ids,
            });
        }
        Ok(scopes)
    }
    pub async fn publish(&self, db: &mut Db, roots: &BTreeMap<i64, i64>) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let stats = one(
            db,
            "SELECT COALESCE(max(id),0) AS watermark,count(*) AS count FROM rust_sync_stage",
            &[],
        )
        .await?;
        let now = time::now();
        exec(db,"INSERT INTO rust_sync_state(id,watermark,last_incremental,last_reconciled,scanned_rows,mode) VALUES(1,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET watermark=excluded.watermark,last_incremental=excluded.last_incremental,last_reconciled=excluded.last_reconciled,scanned_rows=excluded.scanned_rows,mode=excluded.mode",&[
            json!(self.watermark.max(int(&stats,"watermark"))),json!(now),
            json!(if self.reconcile() { &now } else { &self.last_reconciled }),stats["count"].clone(),json!(self.mode)]).await?;
        if let Some(fingerprints) = &self.fingerprints {
            exec(db, "DELETE FROM rust_usage_fingerprints", &[]).await?;
            for chunk in fingerprints.chunks(256) {
                let mut q = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                    "INSERT INTO rust_usage_fingerprints(account_id,day,record_count,amount,digest) ",
                );
                q.push_values(chunk, |mut row, f| {
                    row.push_bind(f.account_id)
                        .push_bind(&f.day)
                        .push_bind(f.count)
                        .push_bind(&f.amount)
                        .push_bind(&f.digest);
                });
                q.build().execute(&mut *db).await?;
            }
        }
        for chunk in roots.iter().collect::<Vec<_>>().chunks(256) {
            let mut q = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                "INSERT INTO rust_sync_roots(account_id,root_id) ",
            );
            q.push_values(chunk, |mut row, (account, root)| {
                row.push_bind(**account).push_bind(**root);
            });
            q.push(" ON CONFLICT(account_id) DO UPDATE SET root_id=excluded.root_id");
            q.build().execute(&mut *db).await?;
        }
        Ok(())
    }
}
