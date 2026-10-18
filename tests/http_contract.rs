#[path = "support/http_contract.rs"]
mod http_contract;

#[tokio::test]
async fn route_boundaries() {
    http_contract::run_workflow("route-boundaries")
        .await
        .unwrap_or_else(|error| panic!("{error}"));
}

macro_rules! workflow_test {
    ($test:ident, $fixture:literal) => {
        #[tokio::test]
        async fn $test() {
            http_contract::run_workflow($fixture)
                .await
                .unwrap_or_else(|error| panic!("{error}"));
        }
    };
}

macro_rules! v011_workflow_test {
    ($test:ident, $fixture:literal) => {
        #[tokio::test]
        async fn $test() {
            http_contract::run_workflow_v011($fixture)
                .await
                .unwrap_or_else(|error| panic!("{error}"));
        }
    };
}

workflow_test!(identity_errors, "identity-errors");
workflow_test!(identity_session_boundaries, "identity-session-boundaries");
workflow_test!(
    identity_token_refresh_expiry,
    "identity-token-refresh-expiry"
);
workflow_test!(identity_verifier_outage, "identity-verifier-outage");
workflow_test!(identity_tamper_rejection, "identity-tamper-rejection");
workflow_test!(cleanup_partial_parent_scope, "cleanup-partial-parent-scope");
workflow_test!(cleanup_partial_shadow_scope, "cleanup-partial-shadow-scope");
workflow_test!(
    cleanup_partial_nested_shadow_scope,
    "cleanup-partial-nested-shadow-scope"
);
workflow_test!(cleanup_count_evidence, "cleanup-count-evidence");
workflow_test!(cleanup_deferred_restart, "cleanup-deferred-restart");
workflow_test!(cleanup_gap_restart, "cleanup-gap-restart");
workflow_test!(cleanup_before_coverage, "cleanup-before-coverage");
workflow_test!(cleanup_no_account_borrow, "cleanup-no-account-borrow");
workflow_test!(cleanup_no_user_borrow, "cleanup-no-user-borrow");
workflow_test!(cleanup_evidence_not_reused, "cleanup-evidence-not-reused");
workflow_test!(
    cleanup_count_exceeds_evidence,
    "cleanup-count-exceeds-evidence"
);
workflow_test!(cleanup_end_boundary, "cleanup-end-boundary");
workflow_test!(cleanup_gap_drift_locality, "cleanup-gap-drift-locality");
workflow_test!(
    cleanup_stale_sync_masks_open,
    "cleanup-stale-sync-masks-open"
);
workflow_test!(
    account_state_overview_role_revision,
    "account-state-overview-role-revision"
);
v011_workflow_test!(starts_costs, "starts-costs");
workflow_test!(start_beijing_boundaries, "start-beijing-boundaries");
workflow_test!(expenses_netting, "expenses-netting");
v011_workflow_test!(billing_proportional, "billing-proportional");
v011_workflow_test!(billing_matched_only, "billing-matched-only");
workflow_test!(paid_upgrade, "paid-upgrade");
workflow_test!(deletion_jobs, "deletion-jobs");
workflow_test!(stale_preview_drift, "stale-preview-drift");
workflow_test!(atomic_rollback, "atomic-rollback");
workflow_test!(legacy_restart, "legacy-restart");
workflow_test!(old_schema_migration, "old-schema-migration");
workflow_test!(concurrency, "concurrency");
