use crate::error::{Error, Result};
use std::{net::SocketAddr, path::PathBuf};
#[derive(Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub source_url: String,
    pub fixture: Option<PathBuf>,
    pub origin: String,
    pub verify_url: String,
    pub bind: SocketAddr,
    pub background: bool,
    pub read_only: bool,
    pub external_writes: bool,
    pub test: bool,
    pub revision: String,
    pub assets: PathBuf,
}
impl Config {
    pub fn env() -> Result<Self> {
        let get = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
        let test = get("SETTLEMENT_ENV", "production") == "test";
        let fixture = std::env::var("SOURCE_FIXTURE")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        if fixture.is_some() && !test {
            return Err(Error::invalid("测试数据源仅限隔离测试环境"));
        }
        let origin = get("PUBLIC_ORIGIN", "https://example.invalid")
            .trim_end_matches('/')
            .to_owned();
        let verify_url = get("SUB2API_VERIFY_URL", "http://sub2api:8080")
            .trim_end_matches('/')
            .to_owned();
        Ok(Self {
            data_dir: PathBuf::from(get("DATA_DIR", "data")),
            source_url: get("SOURCE_DATABASE_URL", ""),
            fixture,
            origin,
            verify_url,
            bind: get("SETTLEMENT_BIND", "0.0.0.0:18082")
                .parse()
                .map_err(|_| Error::invalid("监听地址无效"))?,
            background: get("SETTLEMENT_BACKGROUND", "true") != "false",
            read_only: get("SETTLEMENT_READ_ONLY", "false") == "true",
            external_writes: get("SETTLEMENT_EXTERNAL_WRITES", "true") != "false"
                && get("SETTLEMENT_READ_ONLY", "false") != "true",
            test,
            revision: get("APP_REVISION", "development"),
            assets: PathBuf::from(get("SETTLEMENT_ASSETS", "frontend/dist")),
        })
    }
}
