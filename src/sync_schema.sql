CREATE TABLE IF NOT EXISTS rust_sync_state (
    id INTEGER PRIMARY KEY CHECK(id=1),
    watermark INTEGER NOT NULL DEFAULT 0,
    last_incremental TEXT NOT NULL,
    last_reconciled TEXT NOT NULL,
    scanned_rows INTEGER NOT NULL DEFAULT 0,
    mode TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS rust_usage_fingerprints (
    account_id INTEGER NOT NULL,
    day TEXT NOT NULL,
    record_count INTEGER NOT NULL,
    amount TEXT NOT NULL,
    digest TEXT NOT NULL,
    PRIMARY KEY(account_id,day)
);
CREATE TABLE IF NOT EXISTS rust_sync_roots (
    account_id INTEGER PRIMARY KEY,
    root_id INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS rust_usage_account_time ON usage(account_id,created_at,id);
