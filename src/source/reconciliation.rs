use super::*;
use crate::json::digest;
use num_bigint::BigUint;
use std::collections::BTreeMap;

#[derive(serde::Serialize)]
struct CanonicalSourceRow<'a> {
    account_id: i64,
    amount: &'a str,
    cache_creation_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    created_at: &'a str,
    id: i64,
    input_tokens: Option<i64>,
    model: Option<&'a str>,
    output_tokens: Option<i64>,
    request_id: Option<&'a str>,
    requested_model: Option<&'a str>,
    upstream_model: Option<&'a str>,
    upstream_request_id: Option<&'a str>,
    upstream_response_model: Option<&'a str>,
    user_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub account_id: i64,
    pub day: String,
    pub count: i64,
    pub amount: String,
    pub digest: String,
}

impl Snapshot {
    /// Daily reconciliation reads compact fingerprints in the same read-only
    /// snapshot as the subsequent cursor. Only changed buckets are transferred.
    pub async fn fingerprints(&mut self) -> Result<Vec<Fingerprint>> {
        match &mut self.kind {
            SnapshotKind::Pg(tx) => {
                sqlx::query("SET LOCAL statement_timeout=90000")
                    .execute(&mut **tx)
                    .await?;
                let rows = sqlx::query(r#"
                    WITH hashed AS (
                      SELECT account_id,created_at,total_cost,
                        encode(sha256(convert_to(jsonb_build_array(
                          id,account_id,user_id,total_cost::text,
                          to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US'),
                          request_id,upstream_request_id,model,requested_model,upstream_model,
                          upstream_response_model,input_tokens,output_tokens,cache_read_tokens,
                          cache_creation_tokens)::text,'UTF8')),'hex') AS h
                      FROM usage_logs
                    )
                    SELECT account_id, (created_at AT TIME ZONE 'UTC')::date::text AS day,
                        count(*) AS count, sum(total_cost)::text AS amount,
                        encode(sha256(convert_to(concat('v1:',
                          sum(('x'||substr(h,1,16))::bit(64)::bigint::numeric),':',
                          sum(('x'||substr(h,17,16))::bit(64)::bigint::numeric),':',
                          sum(('x'||substr(h,33,16))::bit(64)::bigint::numeric),':',
                          sum(('x'||substr(h,49,16))::bit(64)::bigint::numeric)), 'UTF8')),'hex') AS digest
                    FROM hashed GROUP BY account_id,(created_at AT TIME ZONE 'UTC')::date
                    ORDER BY account_id,day
                "#).fetch_all(&mut **tx).await?;
                sqlx::query("SET LOCAL statement_timeout=15000")
                    .execute(&mut **tx)
                    .await?;
                rows.iter()
                    .map(|row| {
                        Ok(Fingerprint {
                            account_id: row.try_get("account_id")?,
                            day: row.try_get("day")?,
                            count: row.try_get("count")?,
                            amount: row.try_get("amount")?,
                            digest: row.try_get("digest")?,
                        })
                    })
                    .collect()
            }
            SnapshotKind::Fixture(stream) => {
                stream.unchanged()?;
                let mut input = FixtureStream::open(stream.path.clone(), Filter::Scopes(None))?;
                // Commutative record hashes keep memory proportional to buckets,
                // not usage count. These are rescan hints, never financial digests.
                type Bucket = (Decimal, i64, BigUint);
                let mask = (BigUint::from(1u8) << 256usize) - BigUint::from(1u8);
                let mut buckets: BTreeMap<(i64, String), Bucket> = BTreeMap::new();
                while let Some(rows) = input.receiver.recv().await.transpose()? {
                    for row in rows {
                        let amount = Decimal::parse(&row.amount)?;
                        let key = (row.account_id, row.created_at[..10].to_owned());
                        let canonical = CanonicalSourceRow {
                            account_id: row.account_id,
                            amount: &row.amount,
                            cache_creation_tokens: row.detail.cache_creation_tokens,
                            cache_read_tokens: row.detail.cache_read_tokens,
                            created_at: &row.created_at,
                            id: row.id,
                            input_tokens: row.detail.input_tokens,
                            model: row.detail.model.as_deref(),
                            output_tokens: row.detail.output_tokens,
                            request_id: row.detail.request_id.as_deref(),
                            requested_model: row.detail.requested_model.as_deref(),
                            upstream_model: row.detail.upstream_model.as_deref(),
                            upstream_request_id: row.detail.upstream_request_id.as_deref(),
                            upstream_response_model: row.detail.upstream_response_model.as_deref(),
                            user_id: row.user_id,
                        };
                        let bucket = buckets
                            .entry(key)
                            .or_insert_with(|| (Decimal::zero(), 0, BigUint::from(0u8)));
                        bucket.0.add(&amount);
                        bucket.1 += 1;
                        bucket.2 = (&bucket.2
                            + BigUint::parse_bytes(digest(&canonical).as_bytes(), 16)
                                .ok_or_else(Error::unavailable)?)
                            & &mask;
                    }
                }
                input.unchanged()?;
                stream.unchanged()?;
                Ok(buckets
                    .into_iter()
                    .map(|((account_id, day), (sum, count, hash))| Fingerprint {
                        account_id,
                        day,
                        count,
                        amount: sum.to_string(),
                        digest: format!("v1:{hash:064x}"),
                    })
                    .collect())
            }
        }
    }

    pub async fn restrict(&mut self, mut scopes: Vec<Scope>) -> Result<()> {
        for scope in &mut scopes {
            scope.start = time::instant(&scope.start)?;
            scope.end = time::instant(&scope.end)?;
            scope.known_ids.sort_unstable();
            scope.known_ids.dedup();
        }
        self.reset(Filter::Scopes(Some(scopes))).await
    }

    pub async fn unrestrict(&mut self) -> Result<()> {
        self.reset(Filter::Scopes(None)).await
    }

    async fn reset(&mut self, filter: Filter) -> Result<()> {
        match &mut self.kind {
            SnapshotKind::Pg(tx) => {
                sqlx::query("CLOSE settlement_usage")
                    .execute(&mut **tx)
                    .await?;
                declare_cursor(tx, &filter).await
            }
            SnapshotKind::Fixture(stream) => {
                stream.unchanged()?;
                *stream = FixtureStream::open(stream.path.clone(), filter)?;
                Ok(())
            }
        }
    }
}
