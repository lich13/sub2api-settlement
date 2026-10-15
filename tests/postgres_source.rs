use std::{str::FromStr, time::Duration};

use settlement::{
    config::Config,
    source::{Fingerprint, Scope, Snapshot, Source},
};
use sqlx::{
    ConnectOptions, PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

type TestResult<T> = std::result::Result<T, String>;

// TEST_POSTGRES_URL must point to a disposable, isolated PostgreSQL test
// database. The fixture owner creates one temporary schema and one temporary
// login role there; Source itself only receives column-level SELECT grants.
#[tokio::test]
async fn postgres_source_uses_scoped_repeatable_read_only_cursor() {
    let Ok(url) = std::env::var("TEST_POSTGRES_URL") else {
        println!("SKIP: set TEST_POSTGRES_URL to an isolated PostgreSQL test database");
        return;
    };
    if url.trim().is_empty() {
        println!("SKIP: TEST_POSTGRES_URL is empty; use an isolated PostgreSQL test database");
        return;
    }

    if let Err(message) = run_postgres_source_case(&url).await {
        panic!("{message}");
    }
}

struct FixtureNames {
    schema: String,
    role: String,
    application: String,
    user_barrier: i64,
}

impl FixtureNames {
    fn new() -> Self {
        let suffix = Uuid::new_v4().simple().to_string();
        Self {
            schema: format!("settlement_source_{suffix}"),
            role: format!("settlement_ro_{suffix}"),
            application: format!("settlement_source_test_{}", &suffix[..20]),
            user_barrier: Uuid::new_v4().as_u128() as i64,
        }
    }
}

async fn run_postgres_source_case(admin_url: &str) -> TestResult<()> {
    let admin_options = PgConnectOptions::from_str(admin_url)
        .map_err(|error| sql_failure("parse TEST_POSTGRES_URL", &error))?;
    let admin = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(8))
        .connect_with(admin_options.clone())
        .await
        .map_err(|error| sql_failure("connect isolated PostgreSQL test database", &error))?;
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&admin)
        .await
        .map_err(|error| sql_failure("read test database name", &error))?;
    let names = FixtureNames::new();

    let setup = create_fixture(&admin, &admin_options, &database, &names).await;
    let result = match setup {
        Ok(source_url) => match exercise_source(&admin, &source_url, &names).await {
            Ok(()) => verify_schema_startup_validation(&admin, &source_url, &names).await,
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    let cleanup = drop_fixture(&admin, &database, &names).await;
    admin.close().await;

    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(test_error), Ok(())) => Err(test_error),
        (Ok(()), Err(cleanup_error)) => Err(cleanup_error),
        (Err(test_error), Err(cleanup_error)) => Err(format!(
            "{test_error}; fixture cleanup also failed: {cleanup_error}"
        )),
    }
}

async fn create_fixture(
    admin: &PgPool,
    admin_options: &PgConnectOptions,
    database: &str,
    names: &FixtureNames,
) -> TestResult<String> {
    let schema = ident(&names.schema);
    let role = ident(&names.role);
    let database = ident(database);
    let password = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());

    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin)
        .await
        .map_err(|error| sql_failure("create isolated fixture schema", &error))?;
    sqlx::query(&format!(
        "CREATE ROLE {role} LOGIN PASSWORD {}",
        literal(&password)
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("create column-limited source role", &error))?;
    sqlx::query(&format!("GRANT CONNECT ON DATABASE {database} TO {role}"))
        .execute(admin)
        .await
        .map_err(|error| sql_failure("grant test database connection", &error))?;
    sqlx::query(&format!(
        "ALTER ROLE {role} SET search_path TO {schema}, pg_catalog"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("set source role search path", &error))?;
    sqlx::query(&format!(
        "ALTER ROLE {role} SET application_name TO {}",
        literal(&names.application)
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("tag source role sessions", &error))?;

    let ddl = format!(
        r#"
        CREATE TABLE {schema}.accounts (
            id BIGINT PRIMARY KEY,
            name TEXT NOT NULL,
            platform TEXT NOT NULL,
            status TEXT NOT NULL,
            parent_account_id BIGINT NULL,
            created_at TIMESTAMPTZ NOT NULL,
            deleted_at TIMESTAMPTZ NULL,
            api_key TEXT NOT NULL
        );
        CREATE TABLE {schema}.users (
            id BIGINT PRIMARY KEY,
            username TEXT NULL,
            role TEXT NOT NULL,
            status TEXT NOT NULL,
            deleted_at TIMESTAMPTZ NULL,
            private_note TEXT NOT NULL
        );
        CREATE TABLE {schema}.usage_logs (
            id BIGINT PRIMARY KEY,
            account_id BIGINT NOT NULL,
            user_id BIGINT NOT NULL,
            total_cost NUMERIC NOT NULL,
            created_at TIMESTAMPTZ NOT NULL,
            request_id TEXT NULL,
            upstream_request_id TEXT NULL,
            model TEXT NULL,
            requested_model TEXT NULL,
            upstream_model TEXT NULL,
            upstream_response_model TEXT NULL,
            input_tokens BIGINT NULL,
            output_tokens BIGINT NULL,
            cache_read_tokens BIGINT NULL,
            cache_creation_tokens BIGINT NULL,
            api_key TEXT NOT NULL
        );
        CREATE TABLE {schema}.usage_cleanup_tasks (
            id BIGINT PRIMARY KEY,
            filters JSONB NOT NULL,
            deleted_rows BIGINT NOT NULL,
            finished_at TIMESTAMPTZ NULL,
            private_note TEXT NOT NULL
        );
        CREATE FUNCTION {schema}.source_barrier(p_lock_key BIGINT) RETURNS BOOLEAN
        LANGUAGE plpgsql VOLATILE AS $source_test$
        BEGIN
            IF current_setting('transaction_isolation') <> 'repeatable read' THEN
                RAISE EXCEPTION USING ERRCODE = 'P0001', MESSAGE = 'source isolation assertion failed';
            END IF;
            IF current_setting('transaction_read_only') <> 'on' THEN
                RAISE EXCEPTION USING ERRCODE = 'P0001', MESSAGE = 'source read-only assertion failed';
            END IF;
            PERFORM pg_advisory_xact_lock(p_lock_key);
            RETURN TRUE;
        END
        $source_test$;
        ALTER TABLE {schema}.users ENABLE ROW LEVEL SECURITY;
        ALTER TABLE {schema}.usage_logs ENABLE ROW LEVEL SECURITY;
        "#
    );
    sqlx::raw_sql(&ddl)
        .execute(admin)
        .await
        .map_err(|error| sql_failure("create synthetic source schema", &error))?;

    let seed = format!(
        r#"
        INSERT INTO {schema}.accounts
            (id, name, platform, status, parent_account_id, created_at, deleted_at, api_key)
        VALUES
            (101, 'Root fixture account', 'openai', 'active', NULL,
             '2024-12-01T00:00:00+00:00', NULL, 'fixture-only-not-a-secret'),
            (102, 'Child fixture account', 'openai', 'active', 101,
             '2024-12-02T00:00:00+00:00', NULL, 'fixture-only-not-a-secret'),
            (103, 'Unrelated fixture account', 'openai', 'active', NULL,
             '2024-12-03T00:00:00+00:00', NULL, 'fixture-only-not-a-secret');

        INSERT INTO {schema}.users (id, username, role, status, deleted_at, private_note)
        VALUES (1, NULL, 'user', 'active', NULL, 'synthetic fixture row');

        INSERT INTO {schema}.usage_cleanup_tasks (id, filters, deleted_rows, finished_at, private_note)
        VALUES
            (201, '{{"account_ids":[101,102]}}'::jsonb, 3,
             '2026-01-16T00:00:00+00:00', 'synthetic cleanup fixture'),
            (202, '{{"account_ids":[103]}}'::jsonb, 0,
             NULL, 'zero-row cleanup fixture');

        INSERT INTO {schema}.usage_logs
            (id, account_id, user_id, total_cost, created_at, request_id,
             upstream_request_id, model, requested_model, upstream_model,
             upstream_response_model, input_tokens, output_tokens,
             cache_read_tokens, cache_creation_tokens, api_key)
        SELECT
            10000 + g,
            CASE WHEN g % 2 = 0 THEN 101 ELSE 102 END,
            1,
            CASE WHEN g = 1
                 THEN 0.000001234567890123456789::numeric
                 ELSE 1.2300::numeric
            END,
            '2026-01-15T12:34:56.123456+00:00'::timestamptz,
            CASE WHEN g = 1 THEN NULL::text ELSE 'request-' || g::text END,
            CASE WHEN g = 1 THEN 'upstream-fixture-id'::text ELSE NULL::text END,
            CASE WHEN g = 1 THEN 'fixture-model'::text ELSE NULL::text END,
            NULL::text,
            CASE WHEN g = 1 THEN 'upstream-fixture-model'::text ELSE NULL::text END,
            NULL::text,
            CASE WHEN g = 1 THEN 2147483648::bigint ELSE NULL::bigint END,
            CASE WHEN g = 1 THEN 42::bigint ELSE NULL::bigint END,
            NULL::bigint,
            CASE WHEN g = 1 THEN 0::bigint ELSE NULL::bigint END,
            'fixture-only-not-a-secret'
        FROM generate_series(1, 2050) AS fixture_rows(g);

        INSERT INTO {schema}.usage_logs (id, account_id, user_id, total_cost, created_at, model, api_key)
        VALUES
            (30000, 103, 1, 7.0000, '2026-01-15T12:00:00+00:00', 'unrelated-fixture-model', 'fixture-only-not-a-secret'),
            (40000, 102, 1, 8.0000, '2026-03-15T12:00:00+00:00', 'out-of-window-fixture-model', 'fixture-only-not-a-secret');

        CREATE POLICY users_snapshot_barrier ON {schema}.users
            FOR SELECT TO {role}
            USING ({schema}.source_barrier({user_barrier}));
        CREATE POLICY usage_source_read ON {schema}.usage_logs
            FOR SELECT TO {role}
            USING (TRUE);

        GRANT USAGE ON SCHEMA {schema} TO {role};
        GRANT EXECUTE ON FUNCTION {schema}.source_barrier(BIGINT) TO {role};
        GRANT SELECT (id, name, platform, status, parent_account_id, created_at, deleted_at)
            ON {schema}.accounts TO {role};
        GRANT SELECT (id, username, role, status, deleted_at)
            ON {schema}.users TO {role};
        GRANT SELECT (id, account_id, user_id, total_cost, created_at,
                      request_id, upstream_request_id, model, requested_model,
                      upstream_model, upstream_response_model, input_tokens,
                      output_tokens, cache_read_tokens, cache_creation_tokens)
            ON {schema}.usage_logs TO {role};
        GRANT SELECT (id, filters, deleted_rows, finished_at)
            ON {schema}.usage_cleanup_tasks TO {role};
        "#,
        user_barrier = names.user_barrier,
    );
    sqlx::raw_sql(&seed)
        .execute(admin)
        .await
        .map_err(|error| sql_failure("seed synthetic read-only source fixture", &error))?;

    let source_url = admin_options
        .clone()
        .username(&names.role)
        .password(&password)
        .to_url_lossy()
        .to_string();
    Ok(source_url)
}

async fn exercise_source(admin: &PgPool, source_url: &str, names: &FixtureNames) -> TestResult<()> {
    let source = Source::new(&config(source_url))
        .await
        .map_err(|error| format!("Source::new failed with service status {}", error.status))?;
    let result = exercise_source_inner(admin, &source, source_url, names).await;
    source.close().await;
    result
}

async fn exercise_source_inner(
    admin: &PgPool,
    source: &Source,
    source_url: &str,
    names: &FixtureNames,
) -> TestResult<()> {
    let scope = Scope {
        root: 101,
        accounts: vec![101, 102],
        start: "2026-01-01T00:00:00+00:00".to_owned(),
        end: "2026-02-01T00:00:00+00:00".to_owned(),
        known_ids: vec![10001],
    };

    let users_lock = hold_advisory_lock(admin, names.user_barrier).await?;
    let source_for_task = source.clone();
    let scope_for_task = scope.clone();
    let snapshot_task =
        tokio::spawn(async move { source_for_task.snapshot(Some(vec![scope_for_task])).await });

    if let Err(error) = wait_for_source_query(
        admin,
        &names.application,
        "FROM users",
        Duration::from_secs(7),
    )
    .await
    {
        let _ = users_lock.rollback().await;
        snapshot_task.abort();
        let _ = snapshot_task.await;
        return Err(error);
    }

    if let Err(error) = update_fixture_during_snapshot(admin, names).await {
        let _ = users_lock.rollback().await;
        snapshot_task.abort();
        let _ = snapshot_task.await;
        return Err(error);
    }

    let usage_lock = match hold_usage_table_lock(admin, names).await {
        Ok(lock) => lock,
        Err(error) => {
            let _ = users_lock.rollback().await;
            snapshot_task.abort();
            let _ = snapshot_task.await;
            return Err(error);
        }
    };
    if let Err(error) = users_lock.rollback().await {
        let _ = usage_lock.rollback().await;
        snapshot_task.abort();
        let _ = snapshot_task.await;
        return Err(sql_failure("release users snapshot barrier", &error));
    }

    let cursor_query = match wait_for_source_query(
        admin,
        &names.application,
        "DECLARE settlement_usage NO SCROLL CURSOR FOR SELECT",
        Duration::from_secs(7),
    )
    .await
    {
        Ok(query) => query,
        Err(error) => {
            let _ = usage_lock.rollback().await;
            snapshot_task.abort();
            let _ = snapshot_task.await;
            return Err(error);
        }
    };
    if let Err(error) = usage_lock.rollback().await {
        snapshot_task.abort();
        let _ = snapshot_task.await;
        return Err(sql_failure("release usage relation barrier", &error));
    }

    let mut snapshot = match snapshot_task.await {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(error)) => {
            return Err(format!(
                "Source::snapshot failed with service status {}",
                error.status
            ));
        }
        Err(_) => return Err("Source::snapshot task did not complete".to_owned()),
    };

    let cursor_lower = cursor_query.to_ascii_lowercase();
    check(
        cursor_lower.contains("declare settlement_usage no scroll cursor for select"),
        "source cursor was not declared NO SCROLL",
    )?;
    check(
        cursor_query.contains("$1") && cursor_query.contains("$2") && !cursor_query.contains("$3"),
        "scoped cursor did not bind its ranges and known IDs in two parameters",
    )?;
    let cursor_lower = cursor_query.to_ascii_lowercase();
    check(
        cursor_lower.contains("jsonb_to_recordset") && cursor_lower.contains("union select unnest"),
        "scoped cursor did not use the bounded JSON range and known-ID query",
    )?;
    check(
        !cursor_query.contains("2026-01-01T00:00:00"),
        "scope timestamps appeared as SQL text instead of bind parameters",
    )?;

    let root_header = snapshot
        .header
        .accounts
        .iter()
        .find(|account| account["id"] == 101);
    check(
        root_header.is_some_and(|account| account["status"] == "active"),
        "snapshot account header did not retain the pre-change state",
    )?;
    check(
        snapshot.header.users.len() == 1
            && snapshot.header.users[0]["name"] == "用户 #1"
            && snapshot.header.users[0]["status"] == "active",
        "snapshot user header did not preserve null-name and pre-change state",
    )?;
    check(
        snapshot.header.cleanups.len() == 1
            && snapshot.header.cleanups[0]["deleted_rows"] == 3
            && snapshot.header.cleanups[0]["filters"]["account_ids"][0] == 101,
        "cleanup header did not use the granted source fields",
    )?;

    // accounts() uses its own pool and should observe the committed account
    // state while this usage cursor still owns the older snapshot.
    let independent_accounts = source.accounts().await.map_err(|error| {
        format!(
            "Source::accounts failed with service status {}",
            error.status
        )
    })?;
    let independent_root = independent_accounts
        .iter()
        .find(|account| account["id"] == 101);
    check(
        independent_root.is_some_and(|account| account["status"] == "disabled"),
        "independent account read did not observe the committed state",
    )?;

    let first_batch = snapshot
        .next()
        .await
        .map_err(|error| {
            format!(
                "first source fetch failed with service status {}",
                error.status
            )
        })?
        .ok_or_else(|| "first source fetch returned no rows".to_owned())?;
    let second_batch = snapshot
        .next()
        .await
        .map_err(|error| {
            format!(
                "second source fetch failed with service status {}",
                error.status
            )
        })?
        .ok_or_else(|| "second source fetch returned no rows".to_owned())?;
    check(
        first_batch.len() == 2048 && second_batch.len() == 2,
        "usage cursor did not fetch the 2048-row batch followed by the remaining two rows",
    )?;
    let extra_batch = snapshot.next().await.map_err(|error| {
        format!(
            "terminal source fetch failed with service status {}",
            error.status
        )
    })?;
    check(
        extra_batch.is_none(),
        "usage cursor returned rows beyond the fixture scope",
    )?;

    let mut old_rows = first_batch;
    old_rows.extend(second_batch);
    check(
        old_rows.len() == 2050
            && old_rows
                .iter()
                .all(|row| matches!(row["account_id"].as_i64(), Some(101 | 102))),
        "source cursor did not include exactly the root and child account rows",
    )?;
    check(
        old_rows.iter().any(|row| row["id"] == 10001)
            && !old_rows.iter().any(|row| row["id"] == 30000)
            && !old_rows.iter().any(|row| row["id"] == 40000),
        "source cursor did not enforce account and time bounds",
    )?;

    let original = old_rows
        .iter()
        .find(|row| row["id"] == 10001)
        .ok_or_else(|| "original known usage row was absent".to_owned())?;
    check(
        original["amount"] == "0.000001234567890123456789",
        "high-precision usage amount was not preserved",
    )?;
    check(
        original["request_id"].is_null()
            && original["upstream_request_id"] == "upstream-fixture-id"
            && original["model"] == "fixture-model"
            && original["requested_model"].is_null()
            && original["upstream_model"] == "upstream-fixture-model"
            && original["upstream_response_model"].is_null(),
        "nullable usage details or model fields were not preserved",
    )?;
    check(
        original["input_tokens"] == 2_147_483_648_i64
            && original["output_tokens"] == 42
            && original["cache_read_tokens"].is_null()
            && original["cache_creation_tokens"] == 0,
        "nullable or 64-bit token fields were not preserved",
    )?;
    drop(snapshot);

    // The selected record moved to another account and outside the time range
    // after the first snapshot began. Its ID remains in known_ids so a fresh
    // snapshot must return the changed row for re-verification.
    let mut recheck = source.snapshot(Some(vec![scope])).await.map_err(|error| {
        format!(
            "re-verification snapshot failed with service status {}",
            error.status
        )
    })?;
    let new_root = recheck
        .header
        .accounts
        .iter()
        .find(|account| account["id"] == 101);
    check(
        new_root.is_some_and(|account| account["status"] == "disabled"),
        "fresh snapshot did not observe the updated account state",
    )?;
    let mut current_rows = Vec::new();
    while let Some(batch) = recheck.next().await.map_err(|error| {
        format!(
            "re-verification fetch failed with service status {}",
            error.status
        )
    })? {
        current_rows.extend(batch);
    }
    check(
        current_rows.len() == 2050,
        "known ID re-verification changed the expected row count",
    )?;
    let moved = current_rows
        .iter()
        .find(|row| row["id"] == 10001)
        .ok_or_else(|| {
            "known row moved outside its account/time scope was not re-fetched".to_owned()
        })?;
    check(
        moved["account_id"] == 103
            && moved["created_at"]
                .as_str()
                .is_some_and(|value| value.starts_with("2026-03-01"))
            && moved["amount"] == "9.8765"
            && moved["model"] == "moved-fixture-model",
        "known ID re-verification did not return the changed out-of-scope source row",
    )?;
    check(
        !current_rows.iter().any(|row| row["id"] == 30000)
            && !current_rows.iter().any(|row| row["id"] == 40000),
        "re-verification included unrelated account/time rows",
    )?;
    drop(recheck);

    verify_reconciliation_source(admin, source, names).await?;
    verify_column_and_write_permissions(source_url, names).await?;
    verify_fixture_unchanged_by_source(admin, names).await?;
    Ok(())
}

async fn verify_reconciliation_source(
    admin: &PgPool,
    source: &Source,
    names: &FixtureNames,
) -> TestResult<()> {
    let schema = ident(&names.schema);
    let precise_amount = "12345678901234567890.1234567890123456789";
    sqlx::query(&format!(
        "UPDATE {schema}.usage_logs SET total_cost = $1::numeric WHERE id = 30000"
    ))
    .bind(precise_amount)
    .execute(admin)
    .await
    .map_err(|error| sql_failure("set precise reconciliation fixture amount", &error))?;
    sqlx::query(&format!(
        "UPDATE {schema}.usage_logs SET created_at = $1::timestamptz WHERE id = 40000"
    ))
    .bind("2026-01-15T12:34:56.123457+00:00")
    .execute(admin)
    .await
    .map_err(|error| sql_failure("set exclusive reconciliation fixture boundary", &error))?;

    let baseline = source_fingerprints(source).await?;
    let precise = baseline
        .iter()
        .find(|fingerprint| fingerprint.account_id == 103 && fingerprint.day == "2026-01-15")
        .ok_or_else(|| "precise PostgreSQL daily fingerprint was absent".to_owned())?;
    check(
        precise.count == 1 && precise.amount == precise_amount,
        "daily PostgreSQL fingerprint did not preserve count and full numeric precision",
    )?;

    sqlx::query(&format!(
        "UPDATE {schema}.usage_logs SET user_id = 2 WHERE id = 30000"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("change reconciliation fixture user", &error))?;
    let changed_user = source_fingerprints(source).await?;
    let user_bucket = changed_user
        .iter()
        .find(|fingerprint| fingerprint.account_id == 103 && fingerprint.day == "2026-01-15")
        .ok_or_else(|| "user-change PostgreSQL fingerprint was absent".to_owned())?;
    check(
        user_bucket.count == precise.count
            && user_bucket.amount == precise.amount
            && user_bucket.digest != precise.digest,
        "daily fingerprint did not detect a user change with unchanged count and amount",
    )?;

    sqlx::query(&format!(
        "UPDATE {schema}.usage_logs SET input_tokens = 77 WHERE id = 30000"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("change reconciliation fixture token count", &error))?;
    let changed_tokens = source_fingerprints(source).await?;
    let token_bucket = changed_tokens
        .iter()
        .find(|fingerprint| fingerprint.account_id == 103 && fingerprint.day == "2026-01-15")
        .ok_or_else(|| "token-change PostgreSQL fingerprint was absent".to_owned())?;
    check(
        token_bucket.count == precise.count
            && token_bucket.amount == precise.amount
            && token_bucket.digest != user_bucket.digest,
        "daily fingerprint did not detect a token change with unchanged count and amount",
    )?;

    let exact_boundary = "2026-01-15T12:34:56.123456+00:00";
    let mut inclusive = source
        .incremental(12050, exact_boundary)
        .await
        .map_err(|error| {
            format!(
                "incremental source open failed with status {}",
                error.status
            )
        })?;
    let inclusive_ids = snapshot_ids(&mut inclusive).await?;
    check(
        inclusive_ids.contains(&12050)
            && inclusive_ids.contains(&30000)
            && inclusive_ids.contains(&10001),
        "incremental cursor did not include the exact lower timestamp, ID watermark, and later known row",
    )?;
    drop(inclusive);

    let mut exclusive = source
        .incremental(12050, "2026-01-15T12:34:56.123457+00:00")
        .await
        .map_err(|error| {
            format!(
                "exclusive incremental source open failed with status {}",
                error.status
            )
        })?;
    let exclusive_ids = snapshot_ids(&mut exclusive).await?;
    check(
        exclusive_ids == vec![10001, 30000, 40000],
        "incremental cursor did not exclude the watermark row just before since while retaining IDs above the watermark",
    )?;
    drop(exclusive);

    let scope = Scope {
        root: 101,
        accounts: vec![101, 102],
        start: exact_boundary.to_owned(),
        end: "2026-01-15T12:34:56.123457+00:00".to_owned(),
        known_ids: vec![10001],
    };
    let mut restricted = source.snapshot(None).await.map_err(|error| {
        format!(
            "unrestricted source open failed with status {}",
            error.status
        )
    })?;
    restricted
        .restrict(vec![scope])
        .await
        .map_err(|error| format!("source restrict failed with status {}", error.status))?;
    let scoped_ids = snapshot_ids(&mut restricted).await?;
    check(
        scoped_ids.len() == 2050
            && scoped_ids.contains(&10001)
            && scoped_ids.contains(&12050)
            && !scoped_ids.contains(&30000)
            && !scoped_ids.contains(&40000)
            && scoped_ids
                .iter()
                .all(|id| *id == 10001 || (10002..=12050).contains(id)),
        "restricted source returned rows outside the scope and known IDs or violated a time boundary",
    )?;
    restricted
        .unrestrict()
        .await
        .map_err(|error| format!("source unrestrict failed with status {}", error.status))?;
    let all_ids = snapshot_ids(&mut restricted).await?;
    check(
        all_ids.len() == 2052
            && all_ids.contains(&10001)
            && all_ids.contains(&30000)
            && all_ids.contains(&40000),
        "unrestrict did not restore the full PostgreSQL usage stream",
    )?;
    drop(restricted);

    sqlx::query(&format!(
        "UPDATE {schema}.usage_logs SET total_cost = 7.0000, user_id = 1, input_tokens = NULL WHERE id = 30000"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("restore reconciliation usage fixture", &error))?;
    sqlx::query(&format!(
        "UPDATE {schema}.usage_logs SET created_at = '2026-03-15T12:00:00+00:00'::timestamptz WHERE id = 40000"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("restore reconciliation timestamp fixture", &error))?;
    Ok(())
}

async fn source_fingerprints(source: &Source) -> TestResult<Vec<Fingerprint>> {
    let mut snapshot = source.snapshot(None).await.map_err(|error| {
        format!(
            "fingerprint snapshot open failed with status {}",
            error.status
        )
    })?;
    snapshot
        .fingerprints()
        .await
        .map_err(|error| format!("fingerprint read failed with status {}", error.status))
}

async fn snapshot_ids(snapshot: &mut Snapshot) -> TestResult<Vec<i64>> {
    let mut ids = Vec::new();
    while let Some(batch) = snapshot
        .next()
        .await
        .map_err(|error| format!("source stream failed with status {}", error.status))?
    {
        for row in batch {
            ids.push(
                row["id"]
                    .as_i64()
                    .ok_or_else(|| "source row ID was not an integer".to_owned())?,
            );
        }
    }
    Ok(ids)
}

async fn verify_schema_startup_validation(
    admin: &PgPool,
    source_url: &str,
    names: &FixtureNames,
) -> TestResult<()> {
    let schema = ident(&names.schema);
    let role = ident(&names.role);

    sqlx::query(&format!(
        "ALTER TABLE {schema}.usage_logs DROP COLUMN requested_model"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("remove required source fixture column", &error))?;
    expect_source_startup_error(
        source_url,
        "源数据库字段缺失或类型不兼容：usage_logs.requested_model",
    )
    .await?;
    sqlx::query(&format!(
        "ALTER TABLE {schema}.usage_logs ADD COLUMN requested_model TEXT"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("restore required source fixture column", &error))?;
    sqlx::query(&format!(
        "GRANT SELECT (requested_model) ON {schema}.usage_logs TO {role}"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("restore source fixture column grant", &error))?;

    sqlx::query(&format!(
        "ALTER TABLE {schema}.usage_logs ALTER COLUMN requested_model TYPE BIGINT USING NULL::BIGINT"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("change required source fixture column type", &error))?;
    expect_source_startup_error(
        source_url,
        "源数据库字段缺失或类型不兼容：usage_logs.requested_model",
    )
    .await?;
    sqlx::query(&format!(
        "ALTER TABLE {schema}.usage_logs ALTER COLUMN requested_model TYPE TEXT USING NULL::TEXT"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("restore required source fixture column type", &error))?;

    sqlx::query(&format!(
        "REVOKE SELECT (requested_model) ON {schema}.usage_logs FROM {role}"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("remove source fixture column grant", &error))?;
    expect_source_startup_error(
        source_url,
        "源数据库缺少字段读取权限：usage_logs.requested_model",
    )
    .await?;
    sqlx::query(&format!(
        "GRANT SELECT (requested_model) ON {schema}.usage_logs TO {role}"
    ))
    .execute(admin)
    .await
    .map_err(|error| sql_failure("restore source fixture column grant", &error))?;
    Ok(())
}

async fn expect_source_startup_error(source_url: &str, expected: &str) -> TestResult<()> {
    match Source::new(&config(source_url)).await {
        Ok(source) => {
            source.close().await;
            Err(format!(
                "Source::new unexpectedly accepted schema requiring {expected}"
            ))
        }
        Err(error) => check(
            error.status == 503 && error.message.contains(expected),
            format!(
                "Source::new returned an unclear error for {expected}: {}",
                error.message
            ),
        ),
    }
}

async fn verify_column_and_write_permissions(
    restricted_url: &str,
    names: &FixtureNames,
) -> TestResult<()> {
    let schema = ident(&names.schema);
    let restricted_options = PgConnectOptions::from_str(restricted_url)
        .map_err(|error| sql_failure("parse restricted source URL", &error))?;
    let probe = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(8))
        .connect_with(restricted_options)
        .await
        .map_err(|error| sql_failure("connect restricted source role", &error))?;
    let result = async {
        let current_user: String = sqlx::query_scalar("SELECT current_user")
            .fetch_one(&probe)
            .await
            .map_err(|error| sql_failure("verify restricted source identity", &error))?;
        check(
            current_user == names.role,
            "source did not connect with its restricted role",
        )?;

        sqlx::query("SET default_transaction_read_only = off")
            .execute(&probe)
            .await
            .map_err(|error| sql_failure("prepare independent privilege probe", &error))?;
        let mode: String = sqlx::query_scalar("SHOW default_transaction_read_only")
            .fetch_one(&probe)
            .await
            .map_err(|error| sql_failure("verify privilege probe write mode", &error))?;
        check(
            mode == "off",
            "permission probe remained read-only and would mask role grants",
        )?;

        let update = sqlx::query(&format!(
            "UPDATE {schema}.accounts SET status = 'unexpected-fixture-write' WHERE id = 101"
        ))
        .execute(&probe)
        .await;
        let update_code = update.err().and_then(|error| error_code(&error));
        check(
            update_code.as_deref() == Some("42501"),
            "restricted role UPDATE was not rejected by PostgreSQL privilege checks",
        )?;

        let private_read = sqlx::query(&format!(
            "SELECT api_key FROM {schema}.accounts WHERE id = 101"
        ))
        .fetch_one(&probe)
        .await;
        let private_code = private_read.err().and_then(|error| error_code(&error));
        check(
            private_code.as_deref() == Some("42501"),
            "restricted role could read a non-granted source column",
        )
    }
    .await;
    probe.close().await;
    result
}

async fn verify_fixture_unchanged_by_source(
    admin: &PgPool,
    names: &FixtureNames,
) -> TestResult<()> {
    let schema = ident(&names.schema);
    let usage_count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {schema}.usage_logs"))
        .fetch_one(admin)
        .await
        .map_err(|error| sql_failure("verify usage fixture row count", &error))?;
    let account_status: String = sqlx::query_scalar(&format!(
        "SELECT status FROM {schema}.accounts WHERE id = 101"
    ))
    .fetch_one(admin)
    .await
    .map_err(|error| sql_failure("verify account fixture state", &error))?;
    let moved: sqlx::postgres::PgRow = sqlx::query(&format!(
        "SELECT account_id, created_at::text AS created_at, total_cost::text AS amount FROM {schema}.usage_logs WHERE id = 10001"
    ))
    .fetch_one(admin)
    .await
    .map_err(|error| sql_failure("verify moved usage fixture state", &error))?;
    let moved_account: i64 = moved
        .try_get("account_id")
        .map_err(|error| sql_failure("decode moved usage account", &error))?;
    let moved_amount: String = moved
        .try_get("amount")
        .map_err(|error| sql_failure("decode moved usage amount", &error))?;
    check(
        usage_count == 2052
            && account_status == "disabled"
            && moved_account == 103
            && moved_amount == "9.8765",
        "source operations changed the post-fixture PostgreSQL state",
    )
}

async fn update_fixture_during_snapshot(admin: &PgPool, names: &FixtureNames) -> TestResult<()> {
    let schema = ident(&names.schema);
    let mut writer = admin
        .begin()
        .await
        .map_err(|error| sql_failure("begin fixture-only concurrent update", &error))?;
    sqlx::query(&format!(
        "UPDATE {schema}.accounts SET status = 'disabled' WHERE id = 101"
    ))
    .execute(&mut *writer)
    .await
    .map_err(|error| sql_failure("update synthetic account state", &error))?;
    sqlx::query(&format!(
        "UPDATE {schema}.users SET status = 'suspended' WHERE id = 1"
    ))
    .execute(&mut *writer)
    .await
    .map_err(|error| sql_failure("update synthetic user state", &error))?;
    sqlx::query(&format!(
        "UPDATE {schema}.usage_logs SET account_id = 103, created_at = $1::timestamptz, total_cost = $2::numeric, model = $3 WHERE id = 10001"
    ))
    .bind("2026-03-01T00:00:00+00:00")
    .bind("9.8765")
    .bind("moved-fixture-model")
    .execute(&mut *writer)
    .await
    .map_err(|error| sql_failure("move synthetic known usage row", &error))?;
    writer
        .commit()
        .await
        .map_err(|error| sql_failure("commit fixture-only concurrent update", &error))
}

async fn hold_advisory_lock(
    pool: &PgPool,
    key: i64,
) -> TestResult<sqlx::Transaction<'static, sqlx::Postgres>> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| sql_failure("begin source synchronization barrier", &error))?;
    let row = sqlx::query("SELECT pg_advisory_xact_lock($1), TRUE AS acquired")
        .bind(key)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| sql_failure("acquire source synchronization barrier", &error))?;
    let acquired: bool = row
        .try_get("acquired")
        .map_err(|error| sql_failure("decode source synchronization barrier", &error))?;
    check(acquired, "PostgreSQL did not acquire the test barrier")?;
    Ok(transaction)
}

async fn hold_usage_table_lock(
    pool: &PgPool,
    names: &FixtureNames,
) -> TestResult<sqlx::Transaction<'static, sqlx::Postgres>> {
    let schema = ident(&names.schema);
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| sql_failure("begin usage relation barrier", &error))?;
    sqlx::query(&format!(
        "LOCK TABLE {schema}.usage_logs IN ACCESS EXCLUSIVE MODE"
    ))
    .execute(&mut *transaction)
    .await
    .map_err(|error| sql_failure("acquire usage relation barrier", &error))?;
    Ok(transaction)
}

async fn wait_for_source_query(
    admin: &PgPool,
    application: &str,
    fragment: &str,
    timeout: Duration,
) -> TestResult<String> {
    let pattern = format!("%{fragment}%");
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let row = sqlx::query(
            "SELECT query FROM pg_stat_activity WHERE application_name = $1 AND wait_event_type = 'Lock' AND query ILIKE $2 ORDER BY backend_start DESC LIMIT 1",
        )
        .bind(application)
        .bind(&pattern)
        .fetch_optional(admin)
        .await
        .map_err(|error| sql_failure("inspect isolated source query state", &error))?;
        if let Some(row) = row {
            return row
                .try_get("query")
                .map_err(|error| sql_failure("decode isolated source query state", &error));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "source did not reach the expected PostgreSQL lock barrier for {fragment}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn drop_fixture(admin: &PgPool, database: &str, names: &FixtureNames) -> TestResult<()> {
    let schema = ident(&names.schema);
    let role = ident(&names.role);
    let database = ident(database);
    let role_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
            .bind(&names.role)
            .fetch_one(admin)
            .await
            .map_err(|error| sql_failure("check temporary source role before cleanup", &error))?;
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(admin)
        .await
        .map_err(|error| sql_failure("drop isolated fixture schema", &error))?;
    if role_exists {
        sqlx::query(&format!(
            "REVOKE CONNECT ON DATABASE {database} FROM {role}"
        ))
        .execute(admin)
        .await
        .map_err(|error| sql_failure("revoke temporary database connection", &error))?;
        sqlx::query(&format!("DROP ROLE {role}"))
            .execute(admin)
            .await
            .map_err(|error| sql_failure("drop temporary restricted role", &error))?;
    }
    Ok(())
}

fn config(source_url: &str) -> Config {
    Config {
        data_dir: "test-data".into(),
        source_url: source_url.to_owned(),
        fixture: None,
        origin: "https://example.invalid".to_owned(),
        verify_url: "http://127.0.0.1:1".to_owned(),
        bind: "127.0.0.1:0".parse().expect("static loopback address"),
        background: false,
        read_only: true,
        external_writes: false,
        test: true,
        revision: "postgres-source-fixture".to_owned(),
        assets: "test-assets".into(),
    }
}

fn check(condition: bool, message: impl Into<String>) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn error_code(error: &sqlx::Error) -> Option<String> {
    match error {
        sqlx::Error::Database(database) => database.code().map(|code| code.into_owned()),
        _ => None,
    }
}

fn sql_failure(label: &str, error: &sqlx::Error) -> String {
    match error_code(error) {
        Some(code) => format!("{label} failed (SQLSTATE {code})"),
        None => format!("{label} failed (PostgreSQL client/transport error)"),
    }
}
