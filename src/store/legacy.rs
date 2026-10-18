use super::*;

fn quoted_column(column: &Value) -> Result<String> {
    let name = column["name"]
        .as_str()
        .filter(|name| !name.is_empty() && !name.contains('\0'))
        .ok_or_else(Error::unavailable)?;
    Ok(format!("\"{}\"", name.replace('"', "\"\"")))
}

pub(super) async fn migrate(db: &mut SqliteConnection) -> Result<()> {
    let columns = all(db, "PRAGMA table_info(cycles)", &[]).await?;
    let additions = [
        ("start_mode", "TEXT NOT NULL DEFAULT 'exact'"),
        ("start_lower", "TEXT"),
        ("start_upper", "TEXT"),
        ("start_confirmed", "INTEGER NOT NULL DEFAULT 1"),
        ("start_confirmed_by", "INTEGER"),
        ("start_confirmed_at", "TEXT"),
        ("start_source_usage_id", "INTEGER"),
        ("renewed_from_cycle_id", "INTEGER"),
        ("start_anchor", "TEXT"),
        ("cost_kind", "TEXT NOT NULL DEFAULT 'exact'"),
        ("cost_source", "TEXT NOT NULL DEFAULT 'manual'"),
        ("bill_import_id", "INTEGER"),
        ("bill_end", "TEXT"),
        ("bill_digest", "TEXT"),
        ("bill_difference_confirmed", "INTEGER NOT NULL DEFAULT 0"),
        (
            "bill_allocation_mode",
            "TEXT NOT NULL DEFAULT 'proportional'",
        ),
        ("billable_cost_fen", "INTEGER"),
        ("start_locked", "INTEGER NOT NULL DEFAULT 0"),
    ];
    // Both identifiers and SQL types come exclusively from the fixed list above.
    for (name, kind) in additions {
        if !columns.iter().any(|r| r["name"] == name) {
            exec(
                db,
                &format!("ALTER TABLE cycles ADD COLUMN {name} {kind}"),
                &[],
            )
            .await?;
        }
    }
    exec(
        db,
        "UPDATE cycles SET start_mode='exact' WHERE start_mode IS NULL OR start_mode=''",
        &[],
    )
    .await?;
    exec(
        db,
        "UPDATE cycles SET start_lower=start WHERE start_lower IS NULL",
        &[],
    )
    .await?;
    exec(
        db,
        "UPDATE cycles SET start_confirmed=1 WHERE start_confirmed IS NULL",
        &[],
    )
    .await?;
    for table in ["settlements", "orders"] {
        // Only these code-owned table names may enter the migration SQL.
        debug_assert!(matches!(table, "settlements" | "orders"));
        let columns = all(db, &format!("PRAGMA table_info({table})"), &[]).await?;
        if columns
            .iter()
            .any(|r| r["name"] == "cycle_id" && r["notnull"] == 1)
        {
            let sequence = optional(
                db,
                "SELECT seq FROM sqlite_sequence WHERE name=?",
                &[json!(table)],
            )
            .await?;
            let statement = SCHEMA
                .split(';')
                .map(str::trim)
                .find(|s| s.starts_with(&format!("CREATE TABLE IF NOT EXISTS {table}(")))
                .ok_or_else(Error::unavailable)?;
            exec(
                db,
                &statement.replace(&format!("{table}("), &format!("{table}_v4(")),
                &[],
            )
            .await?;
            // PRAGMA supplies metadata, not SQL: quote every column identifier.
            let names = columns
                .iter()
                .map(quoted_column)
                .collect::<Result<Vec<_>>>()?
                .join(",");
            exec(
                db,
                &format!("INSERT INTO {table}_v4({names}) SELECT {names} FROM {table}"),
                &[],
            )
            .await?;
            exec(db, &format!("DROP TABLE {table}"), &[]).await?;
            exec(
                db,
                &format!("ALTER TABLE {table}_v4 RENAME TO {table}"),
                &[],
            )
            .await?;
            if let Some(seq) = sequence {
                exec(
                    db,
                    "UPDATE sqlite_sequence SET seq=max(seq,?) WHERE name=?",
                    &[seq["seq"].clone(), json!(table)],
                )
                .await?;
            }
        } else if !columns.iter().any(|r| r["name"] == "expense_id") {
            exec(
                db,
                &format!("ALTER TABLE {table} ADD COLUMN expense_id INTEGER"),
                &[],
            )
            .await?;
        }
    }
    if !all(db, "PRAGMA table_info(orders)", &[])
        .await?
        .iter()
        .any(|r| r["name"] == "netting_payment_id")
    {
        exec(db,"ALTER TABLE orders ADD COLUMN netting_payment_id INTEGER REFERENCES netting_payments(id)",&[]).await?;
    }
    sqlx::raw_sql("CREATE INDEX IF NOT EXISTS order_payer ON orders(payer_id,status); CREATE INDEX IF NOT EXISTS order_creditor ON orders(creditor_id,status); CREATE INDEX IF NOT EXISTS order_netting_payment ON orders(netting_payment_id); CREATE TABLE IF NOT EXISTS rust_root_versions(root_id INTEGER PRIMARY KEY, version INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS rust_sync_stage(id INTEGER PRIMARY KEY,account_id INTEGER NOT NULL,root_id INTEGER NOT NULL,user_id INTEGER NOT NULL,amount TEXT NOT NULL,created_at TEXT NOT NULL,detail TEXT); CREATE INDEX IF NOT EXISTS rust_stage_root ON rust_sync_stage(root_id,created_at); CREATE TABLE IF NOT EXISTS rust_source_presence(id INTEGER PRIMARY KEY); CREATE TABLE IF NOT EXISTS rust_sync_changed(root_id INTEGER PRIMARY KEY); CREATE INDEX IF NOT EXISTS usage_account_time ON usage(account_id,created_at); CREATE INDEX IF NOT EXISTS cycle_open ON cycles(status,account_id);").execute(&mut *db).await?;
    Ok(())
}

/// Additive v8 columns also work with unversioned ledgers. Identifiers and types
/// are fixed code-owned values; the released v1 checksum remains unchanged.
pub(super) async fn vendor_columns(db: &mut SqliteConnection) -> Result<()> {
    for (table, fields) in [
        ("profiles", &["vendor_id"][..]),
        ("cycles", &["vendor_id", "vendor_period_id"][..]),
    ] {
        let columns = all(db, &format!("PRAGMA table_info({table})"), &[]).await?;
        for field in fields {
            if !columns.iter().any(|column| column["name"] == *field) {
                exec(
                    db,
                    &format!("ALTER TABLE {table} ADD COLUMN {field} INTEGER"),
                    &[],
                )
                .await?;
            }
        }
    }
    sqlx::raw_sql("CREATE INDEX IF NOT EXISTS profile_vendor ON profiles(vendor_id); CREATE INDEX IF NOT EXISTS cycle_vendor_period ON cycles(vendor_period_id,id);")
        .execute(db).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_columns_are_quoted_identifiers() {
        assert_eq!(quoted_column(&json!({"name":"id"})).unwrap(), "\"id\"");
        assert_eq!(
            quoted_column(&json!({"name":"a\";DROP TABLE orders;--"})).unwrap(),
            "\"a\"\";DROP TABLE orders;--\""
        );
        for column in [
            json!({}),
            json!({"name": null}),
            json!({"name": 1}),
            json!({"name": ""}),
            json!({"name": "a\0b"}),
        ] {
            assert!(quoted_column(&column).is_err());
        }
    }
}
