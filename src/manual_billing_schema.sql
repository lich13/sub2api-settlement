CREATE TABLE manual_bill_bases (
    id INTEGER PRIMARY KEY,
    cycle_id INTEGER REFERENCES cycles(id),
    vendor_period_id INTEGER REFERENCES vendor_periods(id),
    config TEXT NOT NULL,
    result TEXT NOT NULL,
    created_by INTEGER NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL,
    CHECK ((cycle_id IS NULL) != (vendor_period_id IS NULL))
);
CREATE INDEX manual_bill_cycle ON manual_bill_bases(cycle_id,id);
CREATE INDEX manual_bill_vendor ON manual_bill_bases(vendor_period_id,id);
ALTER TABLE cycles ADD COLUMN manual_basis_id INTEGER REFERENCES manual_bill_bases(id);
ALTER TABLE vendor_periods ADD COLUMN manual_basis_id INTEGER REFERENCES manual_bill_bases(id);
CREATE TABLE manual_bill_drafts (
    cycle_id INTEGER UNIQUE REFERENCES cycles(id),
    vendor_period_id INTEGER UNIQUE REFERENCES vendor_periods(id),
    created_at TEXT NOT NULL,
    CHECK ((cycle_id IS NULL) != (vendor_period_id IS NULL))
);
