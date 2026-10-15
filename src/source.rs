use crate::{
    config::Config,
    error::{Error, Result},
    money::Decimal,
    time,
};
use serde::{
    Deserialize,
    de::{IgnoredAny, MapAccess, Visitor},
};
use serde_json::{Value, json};
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::{Mutex, mpsc};

mod reconciliation;
mod schema;
pub use reconciliation::Fingerprint;

#[derive(Clone)]
enum Filter {
    Scopes(Option<Vec<Scope>>),
    Incremental { watermark: i64, since: String },
}

#[derive(Clone, Default, Deserialize)]
pub struct Header {
    #[serde(default)]
    pub accounts: Vec<Value>,
    #[serde(default)]
    pub users: Vec<Value>,
    #[serde(default)]
    pub cleanups: Vec<Value>,
}
#[derive(Clone, Debug)]
pub struct Scope {
    pub root: i64,
    pub accounts: Vec<i64>,
    pub start: String,
    pub end: String,
    pub known_ids: Vec<i64>,
}
type HeaderCache = Option<(SystemTime, u64, Header)>;
#[derive(Clone)]
pub struct Source {
    fixture: Option<PathBuf>,
    pool: Option<PgPool>,
    accounts_pool: Option<PgPool>,
    cache: Arc<Mutex<HeaderCache>>,
}
impl Source {
    pub async fn new(config: &Config) -> Result<Self> {
        if config.fixture.is_some() {
            return Ok(Self {
                fixture: config.fixture.clone(),
                pool: None,
                accounts_pool: None,
                cache: Arc::new(Mutex::new(None)),
            });
        }
        let opts =
            PgConnectOptions::from_str(&config.source_url).map_err(|_| Error::unavailable())?;
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .acquire_timeout(Duration::from_secs(8))
            .after_connect(|c, _| {
                Box::pin(async move {
                    sqlx::query("SET default_transaction_read_only=on")
                        .execute(&mut *c)
                        .await?;
                    sqlx::query("SET statement_timeout=15000")
                        .execute(&mut *c)
                        .await?;
                    Ok(())
                })
            })
            .connect_lazy_with(opts.clone());
        let accounts_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(3))
            .after_connect(|c, _| {
                Box::pin(async move {
                    sqlx::query("SET default_transaction_read_only=on")
                        .execute(&mut *c)
                        .await?;
                    sqlx::query("SET statement_timeout=3000")
                        .execute(&mut *c)
                        .await?;
                    Ok(())
                })
            })
            .connect_lazy_with(opts);
        schema::validate(&pool).await?;
        Ok(Self {
            fixture: None,
            pool: Some(pool),
            accounts_pool: Some(accounts_pool),
            cache: Arc::new(Mutex::new(None)),
        })
    }
    async fn fixture_header(&self, path: &Path) -> Result<Header> {
        let stat = std::fs::metadata(path)?;
        let stamp = stat.modified()?;
        let mut cache = self.cache.lock().await;
        if let Some((s, len, h)) = &*cache
            && *s == stamp
            && *len == stat.len()
        {
            return Ok(h.clone());
        }
        let path = path.to_owned();
        let h = tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(path)?;
            let mut de = serde_json::Deserializer::from_reader(std::io::BufReader::new(file));
            let h = serde::de::Deserializer::deserialize_map(&mut de, HeaderVisitor)
                .map_err(|_| Error::unavailable())?;
            de.end().map_err(|_| Error::unavailable())?;
            Ok::<_, Error>(h)
        })
        .await
        .map_err(|_| Error::unavailable())??;
        *cache = Some((stamp, stat.len(), h.clone()));
        Ok(h)
    }
    pub async fn accounts(&self) -> Result<Vec<Value>> {
        if let Some(path) = &self.fixture {
            return Ok(self.fixture_header(path).await?.accounts);
        }
        let pool = self.accounts_pool.as_ref().ok_or_else(Error::unavailable)?;
        let rows=tokio::time::timeout(Duration::from_secs(4),sqlx::query("SELECT id,name,platform,status,parent_account_id,created_at,deleted_at FROM accounts ORDER BY id").fetch_all(pool)).await.map_err(|_|Error::unavailable())??;
        rows.iter().map(account).collect()
    }
    pub async fn snapshot(&self, scopes: Option<Vec<Scope>>) -> Result<Snapshot> {
        let scopes = scopes
            .map(|scopes| {
                scopes
                    .into_iter()
                    .map(|mut scope| {
                        scope.start = time::instant(&scope.start)?;
                        scope.end = time::instant(&scope.end)?;
                        scope.known_ids.sort_unstable();
                        scope.known_ids.dedup();
                        Ok(scope)
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        self.open(Filter::Scopes(scopes)).await
    }
    pub async fn incremental(&self, watermark: i64, since: &str) -> Result<Snapshot> {
        self.open(Filter::Incremental {
            watermark,
            since: time::instant(since)?,
        })
        .await
    }
    async fn open(&self, filter: Filter) -> Result<Snapshot> {
        if let Some(path) = &self.fixture {
            let header = self.fixture_header(path).await?;
            return Ok(Snapshot {
                header,
                kind: SnapshotKind::Fixture(FixtureStream::open(path.clone(), filter)?),
            });
        }
        let mut tx = self
            .pool
            .as_ref()
            .ok_or_else(Error::unavailable)?
            .begin()
            .await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let accounts=sqlx::query("SELECT id,name,platform,status,parent_account_id,created_at,deleted_at FROM accounts ORDER BY id").fetch_all(&mut *tx).await?.iter().map(account).collect::<Result<Vec<_>>>()?;
        let users=sqlx::query("SELECT id,username AS name,role,status,deleted_at FROM users ORDER BY id").fetch_all(&mut *tx).await?.iter().map(|r|->Result<Value>{let id:i64=r.try_get("id")?;let name:Option<String>=r.try_get("name")?;Ok(json!({"id":id,"name":name.filter(|s|!s.is_empty()).unwrap_or_else(||format!("用户 #{id}")),"role":r.try_get::<String,_>("role")?,"status":r.try_get::<String,_>("status")?,"deleted":r.try_get::<Option<chrono::DateTime<chrono::Utc>>,_>("deleted_at")?.is_some()}))}).collect::<Result<Vec<_>>>()?;
        let cleanups=sqlx::query("SELECT id,filters,deleted_rows,finished_at FROM usage_cleanup_tasks WHERE deleted_rows>0 ORDER BY id").fetch_all(&mut *tx).await?.iter().map(|r|->Result<Value>{Ok(json!({"id":r.try_get::<i64,_>("id")?,"filters":r.try_get::<Value,_>("filters")?,"deleted_rows":r.try_get::<i64,_>("deleted_rows")?,"finished_at":r.try_get::<Option<chrono::DateTime<chrono::Utc>>,_>("finished_at")?.map(time::format)}))}).collect::<Result<Vec<_>>>()?;
        declare_cursor(&mut tx, &filter).await?;
        Ok(Snapshot {
            header: Header {
                accounts,
                users,
                cleanups,
            },
            kind: SnapshotKind::Pg(tx),
        })
    }
    pub async fn close(&self) {
        if let Some(p) = &self.pool {
            p.close().await;
        }
        if let Some(p) = &self.accounts_pool {
            p.close().await;
        }
    }
}

async fn declare_cursor(tx: &mut sqlx::PgConnection, filter: &Filter) -> Result<()> {
    let mut q = sqlx::QueryBuilder::new(
        "DECLARE settlement_usage NO SCROLL CURSOR FOR SELECT id,account_id,user_id,total_cost::text AS amount,created_at,request_id,upstream_request_id,model,requested_model,upstream_model,upstream_response_model,input_tokens,output_tokens,cache_read_tokens,cache_creation_tokens FROM usage_logs",
    );
    if let Filter::Scopes(Some(scopes)) = filter {
        let ranges = scopes
            .iter()
            .map(|s| json!({"accounts":s.accounts,"lower":s.start,"upper":s.end}))
            .collect::<Vec<_>>();
        let mut known = scopes
            .iter()
            .flat_map(|s| s.known_ids.iter().copied())
            .collect::<Vec<_>>();
        known.sort_unstable();
        known.dedup();
        // Two parameters regardless of bucket count, with indexed range/ID
        // lookups. UNION prevents duplicate rows when intervals intersect.
        q.push(" WHERE id IN (SELECT u.id FROM jsonb_to_recordset(")
            .push_bind(json!(ranges))
            .push("::jsonb) AS r(accounts bigint[],lower timestamptz,upper timestamptz) JOIN usage_logs u ON u.account_id=ANY(r.accounts) AND u.created_at>=r.lower AND u.created_at<r.upper UNION SELECT unnest(")
            .push_bind(known).push("::bigint[]))");
    } else if let Filter::Incremental { watermark, since } = filter {
        q.push(" WHERE id>")
            .push_bind(*watermark)
            .push(" OR created_at>=")
            .push_bind(time::parse(since)?);
    }
    q.push(" ORDER BY id");
    q.build().persistent(false).execute(tx).await?;
    Ok(())
}
fn account(r: &sqlx::postgres::PgRow) -> Result<Value> {
    Ok(
        json!({"id":r.try_get::<i64,_>("id")?,"name":r.try_get::<String,_>("name")?,"platform":r.try_get::<String,_>("platform")?,"status":r.try_get::<String,_>("status")?,"parent_account_id":r.try_get::<Option<i64>,_>("parent_account_id")?,"created_at":time::format(r.try_get("created_at")?),"deleted":r.try_get::<Option<chrono::DateTime<chrono::Utc>>,_>("deleted_at")?.is_some()}),
    )
}
pub struct Snapshot {
    pub header: Header,
    kind: SnapshotKind,
}
enum SnapshotKind {
    Fixture(FixtureStream),
    Pg(sqlx::Transaction<'static, sqlx::Postgres>),
}
impl Snapshot {
    pub async fn next(&mut self) -> Result<Option<Vec<Value>>> {
        match &mut self.kind {
            SnapshotKind::Fixture(stream) => {
                let result = stream.receiver.recv().await.transpose()?;
                if result.is_none() {
                    stream.unchanged()?;
                }
                Ok(result)
            }
            SnapshotKind::Pg(tx) => {
                let rows = sqlx::query("FETCH FORWARD 2048 FROM settlement_usage")
                    .fetch_all(&mut **tx)
                    .await?;
                if rows.is_empty() {
                    return Ok(None);
                }
                let mut output = Vec::with_capacity(rows.len());
                for r in rows {
                    let id: i64 = r.try_get("id")?;
                    let mut item = json!({"id":id,"account_id":r.try_get::<i64,_>("account_id")?,"user_id":r.try_get::<i64,_>("user_id")?,"amount":Decimal::parse(&r.try_get::<String,_>("amount")?)?.to_string(),"created_at":time::format(r.try_get("created_at")?)});
                    for key in crate::billing::DETAIL_FIELDS {
                        item[key] = if crate::billing::TOKEN_FIELDS.contains(&key) {
                            r.try_get::<Option<i32>, _>(key)
                                .map(|v| json!(v))
                                .or_else(|_| r.try_get::<Option<i64>, _>(key).map(|v| json!(v)))?
                        } else {
                            json!(r.try_get::<Option<String>, _>(key)?)
                        };
                    }
                    output.push(item);
                }
                Ok(Some(output))
            }
        }
    }
}
struct FixtureStream {
    path: PathBuf,
    stamp: (SystemTime, u64),
    receiver: mpsc::Receiver<Result<Vec<Value>>>,
}
impl FixtureStream {
    fn open(path: PathBuf, filter: Filter) -> Result<Self> {
        let stat = std::fs::metadata(&path)?;
        let stamp = (stat.modified()?, stat.len());
        let (sender, receiver) = mpsc::channel(2);
        let input = path.clone();
        tokio::task::spawn_blocking(move || {
            let result = (|| -> Result<()> {
                let file = std::fs::File::open(input)?;
                let mut reader = std::io::BufReader::with_capacity(256 * 1024, file);
                read_fixture_usage(&mut reader, &sender, &filter)
            })();
            if let Err(error) = result {
                let _ = sender.blocking_send(Err(error));
            }
        });
        Ok(Self {
            path,
            stamp,
            receiver,
        })
    }
    fn unchanged(&self) -> Result<()> {
        let stat = std::fs::metadata(&self.path)?;
        if (stat.modified()?, stat.len()) != self.stamp {
            return Err(Error::unavailable());
        }
        Ok(())
    }
}
struct HeaderVisitor;
impl<'de> Visitor<'de> for HeaderVisitor {
    type Value = Header;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("fixture object")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> std::result::Result<Header, M::Error> {
        let mut h = Header::default();
        while let Some(k) = map.next_key::<String>()? {
            match k.as_str() {
                "accounts" => h.accounts = map.next_value()?,
                "users" => h.users = map.next_value()?,
                "cleanups" => h.cleanups = map.next_value()?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(h)
    }
}
#[derive(Deserialize, serde::Serialize)]
struct FixtureUsage {
    id: i64,
    account_id: i64,
    user_id: i64,
    amount: String,
    created_at: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    upstream_request_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    requested_model: Option<String>,
    #[serde(default)]
    upstream_model: Option<String>,
    #[serde(default)]
    upstream_response_model: Option<String>,
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
    #[serde(default)]
    cache_read_tokens: Option<i64>,
    #[serde(default)]
    cache_creation_tokens: Option<i64>,
}

// Read one bounded JSON value into a reusable buffer, then deserialize from a
// contiguous slice. This keeps fixture streams bounded without per-byte serde
// I/O overhead. Production reads use PostgreSQL's indexed cursor.
fn json_value<R: std::io::BufRead>(reader: &mut R, out: &mut Vec<u8>) -> Result<()> {
    out.clear();
    let first = nonspace(reader)?.ok_or_else(Error::unavailable)?;
    out.push(first);
    let mut quoted = first == b'"';
    let mut escaped = false;
    let container = first == b'{' || first == b'[';
    let mut depth = usize::from(container);
    loop {
        let input = reader.fill_buf()?;
        if input.is_empty() {
            return if depth == 0 && !quoted {
                Ok(())
            } else {
                Err(Error::unavailable())
            };
        }
        let mut consumed = 0;
        let mut complete = false;
        for &byte in input {
            if !container && first != b'"' && (byte.is_ascii_whitespace() || b",]}".contains(&byte))
            {
                complete = true;
                break;
            }
            consumed += 1;
            if quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quoted = false;
                    if !container {
                        complete = true;
                        break;
                    }
                }
            } else {
                match byte {
                    b'"' => quoted = true,
                    b'[' | b'{' => depth += 1,
                    b']' | b'}' if container => {
                        depth = depth.checked_sub(1).ok_or_else(Error::unavailable)?;
                        if depth == 0 {
                            complete = true;
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        out.extend_from_slice(&input[..consumed]);
        reader.consume(consumed);
        if out.len() > 64 * 1024 * 1024 {
            return Err(Error::unavailable());
        }
        if complete {
            return Ok(());
        }
    }
}
fn nonspace<R: std::io::BufRead>(reader: &mut R) -> Result<Option<u8>> {
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(None);
        }
        if let Some(i) = bytes.iter().position(|c| !c.is_ascii_whitespace()) {
            let byte = bytes[i];
            reader.consume(i + 1);
            return Ok(Some(byte));
        }
        let len = bytes.len();
        reader.consume(len);
    }
}
fn expect_byte<R: std::io::BufRead>(reader: &mut R, byte: u8) -> Result<()> {
    if nonspace(reader)? != Some(byte) {
        return Err(Error::unavailable());
    }
    Ok(())
}
fn peek_nonspace<R: std::io::BufRead>(reader: &mut R) -> Result<Option<u8>> {
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(None);
        }
        if let Some(i) = bytes.iter().position(|c| !c.is_ascii_whitespace()) {
            let byte = bytes[i];
            reader.consume(i);
            return Ok(Some(byte));
        }
        let len = bytes.len();
        reader.consume(len);
    }
}
fn read_fixture_usage<R: std::io::BufRead>(
    reader: &mut R,
    sender: &mpsc::Sender<Result<Vec<Value>>>,
    filter: &Filter,
) -> Result<()> {
    expect_byte(reader, b'{')?;
    let mut seen = false;
    let mut raw = Vec::with_capacity(2048);
    let mut batch = Vec::with_capacity(2048);
    loop {
        json_value(reader, &mut raw)?;
        let key: String = serde_json::from_slice(&raw)?;
        expect_byte(reader, b':')?;
        if key == "usage" {
            if seen {
                return Err(Error::unavailable());
            }
            seen = true;
            expect_byte(reader, b'[')?;
            if peek_nonspace(reader)? == Some(b']') {
                expect_byte(reader, b']')?;
            } else {
                loop {
                    json_value(reader, &mut raw)?;
                    let row: FixtureUsage = serde_json::from_slice(&raw)?;
                    let included = if let Filter::Scopes(Some(scopes)) = filter {
                        if scopes
                            .iter()
                            .any(|s| s.known_ids.binary_search(&row.id).is_ok())
                        {
                            true
                        } else if scopes.iter().any(|s| s.accounts.contains(&row.account_id)) {
                            let at = time::instant(&row.created_at)?;
                            scopes.iter().any(|s| {
                                s.accounts.contains(&row.account_id) && at >= s.start && at < s.end
                            })
                        } else {
                            false
                        }
                    } else if let Filter::Incremental { watermark, since } = filter {
                        row.id > *watermark || time::instant(&row.created_at)? >= *since
                    } else {
                        true
                    };
                    if included {
                        batch.push(serde_json::to_value(row)?);
                        if batch.len() == 2048 {
                            sender
                                .blocking_send(Ok(std::mem::take(&mut batch)))
                                .map_err(|_| Error::unavailable())?;
                        }
                    }
                    match nonspace(reader)? {
                        Some(b',') => {}
                        Some(b']') => break,
                        _ => return Err(Error::unavailable()),
                    }
                }
            }
        } else {
            json_value(reader, &mut raw)?;
            let _: IgnoredAny = serde_json::from_slice(&raw)?;
        }
        match nonspace(reader)? {
            Some(b',') => {}
            Some(b'}') => break,
            _ => return Err(Error::unavailable()),
        }
    }
    if !seen || nonspace(reader)?.is_some() {
        return Err(Error::unavailable());
    }
    if !batch.is_empty() {
        sender
            .blocking_send(Ok(batch))
            .map_err(|_| Error::unavailable())?;
    }
    Ok(())
}
