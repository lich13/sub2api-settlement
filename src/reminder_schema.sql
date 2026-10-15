CREATE TABLE IF NOT EXISTS rust_reminders (
    id TEXT PRIMARY KEY,
    user_a INTEGER NOT NULL,
    user_b INTEGER NOT NULL,
    reminder_day TEXT NOT NULL,
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','sending','sent','obsolete','failed')),
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt TEXT NOT NULL,
    created_at TEXT NOT NULL,
    sent_at TEXT,
    error TEXT,
    UNIQUE(user_a,user_b,reminder_day)
);
CREATE INDEX IF NOT EXISTS rust_reminder_due ON rust_reminders(status,next_attempt);
