# Contributing

Discuss changes that affect accounting rules before implementation. Keep fixes focused and include a regression test for the affected behavior. Use synthetic users, accounts and request data only.

## Development

Use the pinned Rust, Node and pnpm versions and preserve both lockfiles. The `settlement-tools demo` command in the README provides a loopback-only environment. No production access is needed for development or tests.

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
pnpm --dir frontend test
pnpm --dir frontend build
cargo build --locked -p settlement-tools
pnpm --dir frontend exec playwright install chromium
pnpm --dir frontend test:e2e
docker build -t settlement-local .
```

The PostgreSQL integration test additionally uses `TEST_POSTGRES_URL`, pointing only to a disposable test database. CI provisions this service. Test fixtures must never contain production ledgers, credentials, sessions, request bodies, email addresses or infrastructure addresses.

Playwright starts a separate Rust demo service and temporary ledger for each scenario. It uses only loopback addresses and synthetic identities, and stops each process after the test. The demo-only `--fail-delete-once` switch exercises an upstream deletion failure without making any external request. Property tests cover allocation invariants and ambiguous CSV matches; retain the generated failure seed when reporting a failure.

## Accounting invariants

- Preserve integer cents, arbitrary-precision source values and largest-remainder allocation, with user ID ascending as the tie-breaker.
- Preserve idempotency signatures, concurrency/version checks, `[start,end)` windows and pairwise netting.
- Keep `tests/golden/` byte-for-byte unchanged. HTTP contracts, legacy SQL ledgers and synthetic session vectors in `tests/fixtures/` are immutable references; new behavior needs explicit tests rather than silent fixture replacement.
- Schema migrations may only add compatible columns, auxiliary tables or indexes. Never recalculate frozen historical amounts.

## Pull requests

Explain the concrete problem, changed behavior, compatibility implications and relevant test results. Include a short Chinese changelog entry for user-visible changes. UI checks should identify synthetic data, viewport and theme; do not include production screenshots.

Run `gitleaks git --config .gitleaks.toml` before publishing a branch. Allowlisting is limited to exact, public synthetic fixture values. Do not add broad exclusions.

Report suspected security vulnerabilities privately through the repository's GitHub Security Advisory feature. Do not put credentials or exploit details in a public issue.
