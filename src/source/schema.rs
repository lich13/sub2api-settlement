use super::*;
use std::collections::BTreeMap;

// Resolve relations through the connection's search_path, matching the actual
// queries. Do not inspect private columns or interpolate database identifiers.
pub(super) async fn validate(pool: &PgPool) -> Result<()> {
    type Field<'a> = (&'a str, &'a [&'a str]);
    type Table<'a> = (&'a str, &'a [Field<'a>]);
    let requirements: &[Table<'_>] = &[
        (
            "accounts",
            &[
                ("id", &["int8"]),
                ("name", &["text", "varchar"]),
                ("platform", &["text", "varchar"]),
                ("status", &["text", "varchar"]),
                ("parent_account_id", &["int8"]),
                ("created_at", &["timestamptz"]),
                ("deleted_at", &["timestamptz"]),
            ],
        ),
        (
            "users",
            &[
                ("id", &["int8"]),
                ("username", &["text", "varchar"]),
                ("role", &["text", "varchar"]),
                ("status", &["text", "varchar"]),
                ("deleted_at", &["timestamptz"]),
            ],
        ),
        (
            "usage_logs",
            &[
                ("id", &["int8"]),
                ("account_id", &["int8"]),
                ("user_id", &["int8"]),
                ("total_cost", &["numeric"]),
                ("created_at", &["timestamptz"]),
                ("request_id", &["text", "varchar"]),
                ("upstream_request_id", &["text", "varchar"]),
                ("model", &["text", "varchar"]),
                ("requested_model", &["text", "varchar"]),
                ("upstream_model", &["text", "varchar"]),
                ("upstream_response_model", &["text", "varchar"]),
                ("input_tokens", &["int4", "int8"]),
                ("output_tokens", &["int4", "int8"]),
                ("cache_read_tokens", &["int4", "int8"]),
                ("cache_creation_tokens", &["int4", "int8"]),
            ],
        ),
        (
            "usage_cleanup_tasks",
            &[
                ("id", &["int8"]),
                ("filters", &["jsonb"]),
                ("deleted_rows", &["int8"]),
                ("finished_at", &["timestamptz"]),
            ],
        ),
    ];
    for (table, fields) in requirements {
        let rows = sqlx::query("SELECT a.attname,t.typname,has_column_privilege(a.attrelid,a.attname,'SELECT') AS readable FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_type t ON t.oid=a.atttypid WHERE a.attrelid=to_regclass($1) AND a.attnum>0 AND NOT a.attisdropped AND a.attname=ANY($2)")
            .bind(table).bind(fields.iter().map(|(name, _)| *name).collect::<Vec<_>>())
            .fetch_all(pool).await
            .map_err(|_| Error::new(503, "源数据库结构检查失败，请检查连接和只读权限"))?;
        let available = rows
            .iter()
            .map(|r| -> Result<_> {
                Ok((
                    r.try_get::<String, _>("attname")?,
                    (
                        r.try_get::<String, _>("typname")?,
                        r.try_get::<bool, _>("readable")?,
                    ),
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        for (field, accepted) in *fields {
            match available.get(*field) {
                Some((kind, true)) if accepted.contains(&kind.as_str()) => {}
                Some((_, false)) => {
                    return Err(Error::new(
                        503,
                        format!("源数据库缺少字段读取权限：{table}.{field}"),
                    ));
                }
                _ => {
                    return Err(Error::new(
                        503,
                        format!(
                            "源数据库字段缺失或类型不兼容：{table}.{field}（需要 {}）",
                            accepted.join(" / ")
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}
