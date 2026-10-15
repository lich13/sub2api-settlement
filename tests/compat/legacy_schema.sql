-- Synthetic pre-v4 table shapes from tests/test_migration_v4.py.
CREATE TABLE cycles(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    cost_fen INTEGER NOT NULL,
    creditor_id INTEGER NOT NULL,
    start TEXT NOT NULL,
    end TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    version INTEGER NOT NULL DEFAULT 1,
    settlement_id INTEGER,
    start_mode TEXT NOT NULL DEFAULT 'exact',
    start_lower TEXT,
    start_upper TEXT,
    start_confirmed INTEGER NOT NULL DEFAULT 1,
    start_confirmed_by INTEGER,
    start_confirmed_at TEXT,
    start_source_usage_id INTEGER,
    renewed_from_cycle_id INTEGER
);
CREATE TABLE settlements(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cycle_id INTEGER NOT NULL,
    snapshot TEXT NOT NULL,
    digest TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    drift INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE TABLE orders(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    settlement_id INTEGER NOT NULL,
    cycle_id INTEGER NOT NULL,
    payer_id INTEGER NOT NULL,
    creditor_id INTEGER NOT NULL,
    payer_name TEXT NOT NULL,
    creditor_name TEXT NOT NULL,
    account_name TEXT NOT NULL,
    amount_fen INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    batch_id INTEGER,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    submitted_at TEXT,
    paid_at TEXT
);
