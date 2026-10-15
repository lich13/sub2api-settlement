CREATE TABLE IF NOT EXISTS rust_payment_notes (
    payment_id INTEGER PRIMARY KEY REFERENCES netting_payments(id),
    note TEXT NOT NULL,
    reference TEXT NOT NULL,
    created_by INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS rust_payment_notes_immutable_update BEFORE UPDATE ON rust_payment_notes BEGIN SELECT RAISE(ABORT,'payment notes are immutable'); END;
CREATE TRIGGER IF NOT EXISTS rust_payment_notes_immutable_delete BEFORE DELETE ON rust_payment_notes BEGIN SELECT RAISE(ABORT,'payment notes are immutable'); END;
