CREATE TABLE IF NOT EXISTS rust_session_keys (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    key_id TEXT NOT NULL
);
