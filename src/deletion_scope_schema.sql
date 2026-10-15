CREATE TABLE IF NOT EXISTS rust_deletion_scopes (
    deletion_id INTEGER PRIMARY KEY REFERENCES account_deletions(id),
    digest TEXT NOT NULL,
    snapshot TEXT NOT NULL
);
