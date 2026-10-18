pub mod backup;
pub mod billing;
pub mod config;
pub mod deletions;
pub mod error;
pub mod http;
pub mod inventory;
pub mod json;
pub mod keys;
pub mod maintenance;
pub mod metrics;
pub mod money;
pub mod rate;
pub mod reminders;
pub mod service;
pub mod source;
pub mod store;
pub mod time;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

mod input;
