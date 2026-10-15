use crate::{
    error::{Error, Result},
    json::{flag, int, text},
    source::Source,
    store::{self, Store},
    time,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock};
#[derive(Clone)]
pub struct AccountSnapshot {
    pub rows: BTreeMap<i64, Value>,
    pub revision: String,
    pub checked_at: Value,
    pub error: Value,
    pub last_attempt: Option<Instant>,
    pub dirty: bool,
}
impl AccountSnapshot {
    pub fn active(&self) -> Vec<Value> {
        self.rows
            .values()
            .rev()
            .filter(|a| !flag(a, "deleted") && a["id"] == a["root_id"])
            .cloned()
            .collect()
    }
    pub fn state(&self) -> Value {
        json!({"revision":self.revision,"checked_at":self.checked_at,"error":self.error,"active_ids":self.active().iter().map(|v|int(v,"id")).collect::<Vec<_>>()})
    }
}
#[derive(Clone)]
pub struct Inventory {
    pub state: Arc<RwLock<AccountSnapshot>>,
    refresh_lock: Arc<Mutex<()>>,
    store: Store,
    source: Source,
}
impl Inventory {
    pub async fn active_snapshot(&self) -> (Value, Vec<Value>) {
        self.refresh(false).await;
        let snapshot = self.state.read().await;
        (snapshot.state(), snapshot.active())
    }
    pub async fn new(store: Store, source: Source) -> Result<Self> {
        let mut db = store.read().await?;
        let rows = store::all(&mut db, "SELECT * FROM accounts", &[])
            .await?
            .into_iter()
            .map(|r| (int(&r, "id"), r))
            .collect();
        Ok(Self {
            store,
            source,
            refresh_lock: Arc::new(Mutex::new(())),
            state: Arc::new(RwLock::new(AccountSnapshot {
                rows,
                revision: String::new(),
                checked_at: Value::Null,
                error: json!("账号状态尚未同步"),
                last_attempt: None,
                dirty: false,
            })),
        })
    }
    pub async fn refresh(&self, force: bool) -> Value {
        if !force
            && self
                .state
                .read()
                .await
                .last_attempt
                .is_some_and(|t| t.elapsed() < Duration::from_secs(4))
        {
            return self.state.read().await.state();
        }
        let guard = if force {
            Some(self.refresh_lock.lock().await)
        } else {
            self.refresh_lock.try_lock().ok()
        };
        let Some(_guard) = guard else {
            return self.state.read().await.state();
        };
        if !force
            && self
                .state
                .read()
                .await
                .last_attempt
                .is_some_and(|t| t.elapsed() < Duration::from_secs(4))
        {
            return self.state.read().await.state();
        }
        self.state.write().await.last_attempt = Some(Instant::now());
        let result=async{
            let raw=self.source.accounts().await?;let current:BTreeMap<_,_>=raw.iter().cloned().map(|a|(int(&a,"id"),a)).collect();
            if current.len()!=raw.len()||current.keys().any(|id|*id<=0){return Err(Error::unavailable());}
            let prior=self.state.read().await.rows.clone();
            let known=prior.iter().map(|(id,a)|(*id,int(a,"root_id"))).collect();
            let roots=roots(&current,&known)?;
            let mut rows:BTreeMap<_,_>=prior.iter().filter(|(id,_)|!current.contains_key(id)).map(|(id,a)|{let mut a=a.clone();a["deleted"]=json!(1);(*id,a)}).collect();
            for (id,a) in current{rows.insert(id,json!({"id":id,"name":a["name"],"platform":a["platform"],"status":a["status"],"deleted":i64::from(flag(&a,"deleted")),"root_id":roots[&id],"created_at":time::instant(text(&a,"created_at"))?}));}
            let stamp=time::now();{let mut state=self.state.write().await;state.dirty|=rows!=prior;state.rows=rows;state.revision=stamp.clone();state.checked_at=json!(stamp);state.error=Value::Null;}
            if self.state.read().await.dirty
                && let Some(mut conn)=self.store.writes.try_acquire()
                    && store::exec(&mut conn,"BEGIN IMMEDIATE",&[]).await.is_ok(){
                        match self.persist(&mut conn).await{Ok(())=>{store::exec(&mut conn,"COMMIT",&[]).await?;self.state.write().await.dirty=false;},Err(e)=>{let _=store::exec(&mut conn,"ROLLBACK",&[]).await;return Err(e);}}
                    }
            Ok::<(),Error>(())
        }.await;
        if result.is_err() {
            self.state.write().await.error = json!("账号状态核对失败，暂保留最近列表");
        }
        self.state.read().await.state()
    }
    pub async fn persist(&self, db: &mut sqlx::SqliteConnection) -> Result<()> {
        let rows = self.state.read().await.rows.clone();
        for row in rows.values() {
            store::exec(db,"INSERT INTO accounts(id,name,root_id,platform,status,deleted,created_at) VALUES(?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,root_id=excluded.root_id,platform=excluded.platform,status=excluded.status,deleted=excluded.deleted",&[row["id"].clone(),row["name"].clone(),row["root_id"].clone(),row["platform"].clone(),row["status"].clone(),row["deleted"].clone(),row["created_at"].clone()]).await?;
        }
        Ok(())
    }
}
pub fn roots(
    current: &BTreeMap<i64, Value>,
    known: &BTreeMap<i64, i64>,
) -> Result<BTreeMap<i64, i64>> {
    let mut roots = BTreeMap::new();
    for id in current.keys() {
        let mut aid = *id;
        let mut seen = HashSet::new();
        while let Some(a) = current.get(&aid) {
            let p = int(a, "parent_account_id");
            if p == 0 {
                break;
            }
            if !seen.insert(aid) {
                return Err(Error::invalid("账号归属循环"));
            }
            aid = p;
        }
        roots.insert(
            *id,
            if current.contains_key(&aid) {
                aid
            } else {
                *known.get(&aid).unwrap_or(&aid)
            },
        );
    }
    Ok(roots)
}
