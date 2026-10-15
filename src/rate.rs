use crate::error::{Error, Result};
use axum::{
    extract::{ConnectInfo, Request},
    http::HeaderMap,
};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub struct Limiter {
    sso: u32,
    writes: u32,
    proxies: Vec<IpAddr>,
    buckets: Mutex<BTreeMap<String, (Instant, u32)>>,
}
impl Limiter {
    pub fn new(sso: u32, writes: u32, proxies: Vec<IpAddr>) -> Self {
        Self {
            sso,
            writes,
            proxies,
            buckets: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn environment(test: bool) -> Result<Self> {
        if test {
            return Ok(Self::new(u32::MAX, u32::MAX, vec![]));
        }
        let read = |name: &str, default: u32| -> Result<u32> {
            std::env::var(name)
                .ok()
                .map(|v| {
                    v.parse::<u32>()
                        .ok()
                        .filter(|n| (1..=100000).contains(n))
                        .ok_or_else(|| Error::invalid(format!("{name} 必须为 1–100000")))
                })
                .unwrap_or(Ok(default))
        };
        let proxies = std::env::var("SETTLEMENT_TRUSTED_PROXIES")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.trim()
                    .parse::<IpAddr>()
                    .map_err(|_| Error::invalid("可信代理必须填写明确 IP 地址"))
            })
            .collect::<Result<Vec<_>>>()?;
        if proxies.len() > 32 {
            return Err(Error::invalid("可信代理配置过多"));
        }
        Ok(Self::new(
            read("SETTLEMENT_SSO_PER_MINUTE", 30)?,
            read("SETTLEMENT_WRITES_PER_MINUTE", 120)?,
            proxies,
        ))
    }
    pub fn client_ip(&self, peer: Option<IpAddr>, headers: &HeaderMap) -> Option<IpAddr> {
        if peer.is_some_and(|ip| ip.is_loopback() || self.proxies.contains(&ip)) {
            headers
                .get("X-Real-IP")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .or(peer)
        } else {
            peer
        }
    }
    pub fn request_ip(&self, request: &Request) -> Option<IpAddr> {
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        self.client_ip(peer, request.headers())
    }
    pub async fn sso(&self, ip: Option<IpAddr>) -> Result<()> {
        let key = ip
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "unknown".into());
        self.take(format!("sso:{key}"), self.sso).await
    }
    pub async fn write(&self, user: i64) -> Result<()> {
        self.take(format!("user:{user}"), self.writes).await
    }
    async fn take(&self, key: String, limit: u32) -> Result<()> {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().await;
        if buckets.len() >= 10000 {
            buckets.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
            if buckets.len() >= 10000 && !buckets.contains_key(&key) {
                return Err(limited());
            }
        }
        let bucket = buckets.entry(key).or_insert((now, 0));
        if now.duration_since(bucket.0) >= Duration::from_secs(60) {
            *bucket = (now, 0);
        }
        if bucket.1 >= limit {
            return Err(limited());
        }
        bucket.1 += 1;
        Ok(())
    }
}
fn limited() -> Error {
    Error::new(429, "请求过于频繁，请稍后重试")
}
