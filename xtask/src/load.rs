use crate::{
    Result,
    demo::{self, Api},
};
use chrono::{Duration as Span, TimeZone, Utc};
use clap::Args;
use serde_json::{Value, json};
use settlement::{json::hash, time};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Args, Clone)]
pub struct Options {
    #[arg(long, default_value_t = 150_000)]
    usage: usize,
    #[arg(long, default_value_t = 50_000)]
    csv_rows: usize,
    #[arg(long, default_value_t = 10)]
    samples: usize,
    /// Run mixed read, preview, matching and idempotent writes for this many seconds.
    #[arg(long, default_value_t = 0)]
    duration: u64,
    /// Number of concurrent synthetic HTTP workers (1..32).
    #[arg(long, default_value_t = 1)]
    workers: usize,
    /// Use a small deterministic data set and exercise source failure/restart recovery.
    #[arg(long)]
    self_check: bool,
}

const START: &str = "2025-01-01T00:00:00.000000+00:00";
const END: &str = "2025-02-01T00:00:00.000000+00:00";

fn fixtures(count: usize, csv_count: usize) -> Result<(Value, String)> {
    let mut source = demo::source("2025-01-20T00:00:00+00:00")?;
    let mut accounts = source["accounts"].as_array().unwrap().clone();
    for id in 5..=12 {
        accounts.push(json!({"id":id,"name":format!("Load account {id}"),"platform":"openai","status":"active","parent_account_id":(id-5)/2+1,"created_at":"2024-12-01T00:00:00+00:00","deleted":false}));
    }
    let mut users = source["users"].as_array().unwrap().clone();
    for id in 4..=64 {
        users.push(
            json!({"id":id,"name":format!("Load user {id}"),"role":"user","status":"active"}),
        );
    }
    let beginning = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
    let usage=(1..=count).map(|id| {
        // Every account is populated; immutable IDs and integer micro-units make regeneration deterministic.
        let account=(id-1)%12+1;
        json!({"id":id,"account_id":account,"user_id":(id-1)%64+1,"amount":format!("{}.{:08}",id%3,id%100000000),"created_at":time::format(beginning+Span::seconds((id/12) as i64)),"request_id":format!("load-{id}"),"model":"load-model","input_tokens":100+(id%31),"output_tokens":20+(id%17),"cache_read_tokens":0,"cache_creation_tokens":0})
    }).collect::<Vec<_>>();
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record([
        "Time",
        "Billed Cost",
        "Model",
        "Input Tokens",
        "Output Tokens",
        "Cache Read Tokens",
        "Cache Creation Tokens",
        "Request ID",
    ])?;
    let matching = usage
        .iter()
        .filter(|r| [2, 7, 8].contains(&r["account_id"].as_i64().unwrap()))
        .collect::<Vec<_>>();
    for index in 0..csv_count {
        let row = matching[index % matching.len()];
        let missing = index % 101 == 0 || index >= matching.len();
        writer.write_record([
            row["created_at"].as_str().unwrap().to_owned(),
            format!("0.{:08}", 10001 + index % 997),
            if missing { "missing" } else { "load-model" }.into(),
            row["input_tokens"].to_string(),
            row["output_tokens"].to_string(),
            "0".into(),
            "0".into(),
            if missing {
                format!("unmatched-{index}")
            } else {
                row["request_id"].as_str().unwrap().into()
            },
        ])?;
    }
    source["accounts"] = json!(accounts);
    source["users"] = json!(users);
    source["usage"] = json!(usage);
    Ok((source, String::from_utf8(writer.into_inner()?)?))
}

fn percentile(values: &[f64], p: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = (sorted.len() - 1) as f64 * p;
    let lower = index.floor() as usize;
    let upper = index.ceil() as usize;
    sorted[lower] + (sorted[upper] - sorted[lower]) * (index - lower as f64)
}
fn metrics(values: &[f64]) -> Value {
    json!({"samples":values.len(),"p50_ms":percentile(values,0.5),"p95_ms":percentile(values,0.95),"mean_ms":values.iter().sum::<f64>()/values.len() as f64})
}

fn units_decimal(units: u128) -> String {
    format!("{}.{:08}", units / 100_000_000, units % 100_000_000)
}

fn assert_observation(name: &str, value: &Value, usage: usize, csv_rows: usize) -> Result<()> {
    if name == "preview" {
        let selected = (1..=usage)
            .filter(|id| [1, 5, 6].contains(&((id - 1) % 12 + 1)))
            .collect::<Vec<_>>();
        let total = selected
            .iter()
            .map(|id| (id % 3) as u128 * 100_000_000 + (id % 100_000_000) as u128)
            .sum::<u128>();
        let calculation = &value["cycles"][0];
        let allocated: i64 = calculation["allocations"]
            .as_array()
            .ok_or("Missing preview allocations")?
            .iter()
            .map(|row| row["amount_fen"].as_i64().unwrap_or_default())
            .sum();
        if calculation["can_settle"] != true
            || calculation["record_count"] != selected.len()
            || calculation["total_consumption"] != units_decimal(total)
            || allocated != 10_000
        {
            return Err(format!("Synthetic preview invariant: allowed={} count={}/{} amount={}/{} fen={allocated}/10000",calculation["can_settle"],calculation["record_count"],selected.len(),calculation["total_consumption"],units_decimal(total)).into());
        }
    }
    if name == "matching" {
        let available = (1..=usage)
            .filter(|id| [2, 7, 8].contains(&((id - 1) % 12 + 1)))
            .count();
        let indices = (0..csv_rows)
            .filter(|index| index % 101 != 0 && *index < available)
            .collect::<Vec<_>>();
        let matched = indices
            .iter()
            .map(|index| (10001 + index % 997) as u128)
            .sum::<u128>();
        let total = (0..csv_rows)
            .map(|index| (10001 + index % 997) as u128)
            .sum::<u128>();
        if value["matched_count"] != indices.len()
            || value["unmatched_count"] != csv_rows - indices.len()
            || value["matched_amount"] != units_decimal(matched)
            || value["total"] != units_decimal(total)
        {
            return Err(
                "Synthetic CSV violates exact matching counts or original-decimal totals".into(),
            );
        }
    }
    Ok(())
}

fn rss_kib() -> Option<u64> {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        return status.lines().find_map(|line| {
            line.strip_prefix("VmHWM:")
                .and_then(|s| s.split_whitespace().next()?.parse().ok())
        });
    }
    let result = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    String::from_utf8(result.stdout).ok()?.trim().parse().ok()
}

async fn edit_cycles(api: &Api) -> Result<Vec<i64>> {
    let overview = api.request("GET", "/overview", Value::Null, "").await?;
    let mut ids = BTreeMap::new();
    for account in overview["accounts"].as_array().ok_or("Missing accounts")? {
        if [1, 2].contains(&account["id"].as_i64().unwrap_or_default()) {
            let cycle = &account["cycle"];
            let id = cycle["id"].as_i64().ok_or("Missing cycle")?;
            api.request("PATCH",&format!("/cycles/{id}"),json!({"account_id":account["id"],"name":cycle["name"],"cost":if account["id"]==1{"100.00"}else{"240.00"},"cost_kind":if account["id"]==1{"exact"}else{"estimated"},"creditor_id":1,"start":START,"start_mode":"exact","version":cycle["version"]}),&format!("load-cycle-{id}")).await?;
            ids.insert(account["id"].as_i64().unwrap(), id);
        }
    }
    Ok(vec![ids[&1], ids[&2]])
}

pub async fn run(mut options: Options) -> Result<()> {
    demo::check_environment()?;
    if options.self_check {
        options.usage = 1200;
        options.csv_rows = 120;
        options.samples = 2;
        options.workers = 2;
    }
    if options.usage < 12
        || options.csv_rows == 0
        || options.csv_rows > 50_000
        || options.samples == 0
        || !(1..=32).contains(&options.workers)
    {
        return Err(
            "Require usage >= 12, 1..50000 CSV rows, positive samples and 1..32 workers".into(),
        );
    }
    let (source, csv) = fixtures(options.usage, options.csv_rows)?;
    let source_hash = hash(&serde_json::to_vec(&source)?);
    let csv_hash = hash(csv.as_bytes());
    let directory = tempfile::tempdir()?;
    let demo_options = demo::Options {
        data_dir: Some(directory.path().into()),
        port: 0,
        assets: PathBuf::from("missing-assets"),
        netting_fixtures: false,
        bill_csv: None,
        self_check: true,
    };
    let server = demo::start(&demo_options, Some(source)).await?;
    let result:Result<Value>=async {
        let api=Api::login_with_timeout(&server.origin,1,Duration::from_secs(300)).await?;
        let ids=edit_cycles(&api).await?;
        let cycles=api.request("GET","/overview",Value::Null,"").await?;
        let version=cycles["accounts"].as_array().unwrap().iter().find(|a|a["id"]==2).ok_or("Missing load cycle")?["cycle"]["version"].clone();
        let tasks=[("preview","POST","/previews".to_owned(),json!({"ids":[ids[0]],"end":END})),("matching","POST",format!("/cycles/{}/bill-previews",ids[1]),json!({"version":version,"end":END,"file_name":"load.csv","csv":csv,"allocation_mode":"proportional"})),("overview","GET","/overview".into(),Value::Null),("account_state","GET","/accounts/state".into(),Value::Null),("orders","GET","/orders".into(),Value::Null)];
        let started=Instant::now();
        let api = Arc::new(api);
        let tasks = Arc::new(tasks);
        let mut workers = tokio::task::JoinSet::new();
        for _ in 0..options.workers {
            let api = api.clone();
            let tasks = tasks.clone();
            let options = options.clone();
            workers.spawn(async move {
                let mut timings:BTreeMap<String,Vec<f64>>=BTreeMap::new();
        let mut iteration=0;
        loop {
            for (name,method,path,payload) in tasks.iter() {
                let timer=Instant::now();
                let response=api.request(method,path,payload.clone(),"load-preview").await?;
                let elapsed = timer.elapsed().as_secs_f64()*1000.0;
                assert_observation(name, &response, options.usage, options.csv_rows)?;
                timings.entry((*name).into()).or_default().push(elapsed);
            }
            if options.duration>0 || options.self_check {
                let payload=json!({"name":"Load retry expense","cost":"0.01","incurred_on":"2025-01-20","creditor_id":1,"allocation_mode":"equal","members":[{"user_id":2,"percent":null} ]});
                let timer=Instant::now();
                let first=api.request("POST","/expenses",payload.clone(),"load-stable-write").await?;
                timings.entry("idempotent_write".into()).or_default().push(timer.elapsed().as_secs_f64()*1000.0);
                let timer=Instant::now();
                let repeated=api.request("POST","/expenses",payload,"load-stable-write").await?;
                timings.entry("idempotent_retry".into()).or_default().push(timer.elapsed().as_secs_f64()*1000.0);
                if first!=repeated { return Err("Idempotency invariant failed".into()); }
            }
            iteration+=1;
            if (options.duration==0 && iteration>=options.samples) || (options.duration>0 && started.elapsed()>=Duration::from_secs(options.duration)) { break; }
        }

                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(timings)
            });
        }
        let mut timings:BTreeMap<String,Vec<f64>>=BTreeMap::new();
        while let Some(worker) = workers.join_next().await {
            for (name, samples) in worker?? {
                timings.entry(name).or_default().extend(samples);
            }
        }
        if options.duration>0 || options.self_check {
            let rows:i64=sqlx::query_scalar("SELECT count(*) FROM expenses WHERE name='Load retry expense'").fetch_one(&server.app.service.store.reads).await?;
            let writes:i64=sqlx::query_scalar("SELECT count(*) FROM requests WHERE key='load-stable-write'").fetch_one(&server.app.service.store.reads).await?;
            if rows!=1 || writes!=1 { return Err("Concurrent retries created duplicate durable writes".into()); }
        }
        let integrity:String=sqlx::query_scalar("PRAGMA integrity_check").fetch_one(&server.app.service.store.reads).await?;
        let foreign=sqlx::query("PRAGMA foreign_key_check").fetch_all(&server.app.service.store.reads).await?;
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM usage").fetch_one(&server.app.service.store.reads).await?;
        if integrity!="ok" || !foreign.is_empty() || count as usize!=options.usage { return Err("Synthetic ledger integrity/count failed".into()); }
        Ok(json!({"engine":"rust","revision":option_env!("APP_REVISION").unwrap_or("development"),"usage":options.usage,"csv_rows":options.csv_rows,"workers":options.workers,"fixture_sha256":source_hash,"csv_sha256":csv_hash,"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,"process_rss_kib":rss_kib(),"memory_scope":"service and in-process synthetic HTTP harness; Linux high-water VmHWM, a single post-run RSS sample elsewhere","duration_seconds":started.elapsed().as_secs_f64(),"wall_operations_per_second":timings.values().map(Vec::len).sum::<usize>() as f64/started.elapsed().as_secs_f64(),"metrics":timings.iter().map(|(k,v)|(k.clone(),metrics(v))).collect::<BTreeMap<_,_>>(),"integrity":"ok"}))
    }.await;
    server.close().await;
    let mut report = result?;
    if options.self_check {
        let server = demo::start(&demo_options, None).await?;
        let recovered: Result<()> = async {
            let source = directory.path().join("source.json");
            let missing = directory.path().join("source.offline");
            std::fs::rename(&source, &missing)?;
            let failed = server.app.service.sync(true).await.is_err();
            std::fs::rename(&missing, &source)?;
            if !failed {
                return Err("Source failure did not fail closed".into());
            }
            server.app.service.sync(true).await?;
            let api = Api::login(&server.origin, 1).await?;
            api.request(
                "POST",
                "/previews",
                json!({"ids":[1],"end":END}),
                "recovery",
            )
            .await?;
            Ok(())
        }
        .await;
        server.close().await;
        recovered?;
        report["recovery"] = json!("source outage and retained-ledger restart passed");
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_generation_and_percentiles_are_reproducible() {
        let a = fixtures(1200, 120).unwrap();
        let b = fixtures(1200, 120).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.0["usage"].as_array().unwrap().len(), 1200);
        assert_eq!(percentile(&[4.0, 1.0, 3.0, 2.0], 0.5), 2.5);
        assert_eq!(a.1.lines().count(), 121);
    }
}
