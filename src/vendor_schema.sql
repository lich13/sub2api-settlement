CREATE TABLE IF NOT EXISTS vendors(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    cost_fen INTEGER NOT NULL CHECK(cost_fen>=0),
    creditor_id INTEGER NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    archived_at TEXT
);
CREATE TABLE IF NOT EXISTS vendor_periods(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vendor_id INTEGER NOT NULL REFERENCES vendors(id),
    name TEXT NOT NULL,
    cost_fen INTEGER NOT NULL CHECK(cost_fen>=0),
    creditor_id INTEGER NOT NULL,
    start TEXT NOT NULL,
    end TEXT,
    start_mode TEXT NOT NULL DEFAULT 'exact',
    start_upper TEXT,
    start_confirmed INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'open',
    version INTEGER NOT NULL DEFAULT 1,
    bill_import_id INTEGER,
    bill_end TEXT,
    bill_digest TEXT,
    bill_difference_confirmed INTEGER NOT NULL DEFAULT 0,
    renewed_from_period_id INTEGER REFERENCES vendor_periods(id),
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS vendor_current_period ON vendor_periods(vendor_id) WHERE status='open' AND end IS NULL;
CREATE INDEX IF NOT EXISTS vendor_period_history ON vendor_periods(vendor_id,id);
CREATE TABLE IF NOT EXISTS vendor_bill_imports(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vendor_period_id INTEGER NOT NULL REFERENCES vendor_periods(id),
    file_name TEXT NOT NULL,
    file_hash TEXT NOT NULL,
    rows TEXT NOT NULL,
    created_by INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(vendor_period_id,file_hash)
);
CREATE TABLE IF NOT EXISTS vendor_bill_previews(
    id TEXT PRIMARY KEY,
    actor_id INTEGER NOT NULL,
    vendor_period_id INTEGER NOT NULL REFERENCES vendor_periods(id),
    payload TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
