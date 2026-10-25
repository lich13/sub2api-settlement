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

## Dependency maintenance

Dependabot checks Cargo, frontend packages, Docker images and GitHub Actions every Monday at 09:00 Asia/Taipei. Ordinary releases have a seven-day cooldown. Version-update PR limits are one for Cargo, three for the frontend, one for Docker and one for Actions. Security alerts and security-update PRs are enabled separately in the repository settings; they do not wait for the ordinary update window or cooldown. All merges remain manual, and merged branches are deleted automatically.

Cargo version updates maintain direct and transitive dependencies within the existing manifest constraints using `lockfile-only`. The frontend separates React, other runtime packages and development tools. Keep `react` and `react-dom` at the same exact version, and update their type packages together. Version-update rules allow minor and patch releases for npm, Docker and Actions; they do not suppress security fixes. Cargo `0.x` minor releases can break compatibility and must stay within the existing manifest range.

Once a month, review the pinned Rust, Node, pnpm, audit and signing tools, CI service images and deferred major releases. Use the same versions locally, in CI and in Docker, retain image digests and Action commit pins, and use supported stable versions. Apply security patches sooner. Refresh the existing lockfiles with `cargo update` and `pnpm --dir frontend update` without `--latest`, then run frozen-lockfile installation, the existing tests and the dependency checks from CI. Keep lockfile changes separate from unrelated features.

The sha2/hmac compatibility-line migration, toml 1.x, jsdom 30 and lucide-react 1.x are deferred until a required fix, feature or end of support justifies them. Evaluate sha2 and hmac together, including their shared digest API. A security fix that requires changing constraints takes priority and needs an explicit compatibility review; never silence it with a new advisory ignore. Preserve the documented RSA exception only while the compiled feature graph excludes both `rsa` and `sqlx-mysql`.

For a failing dependency PR, read all failed checks once. Address shared security failures before retrying unrelated updates; retry infrastructure failures only when their cause has changed. Split a failing group when necessary to isolate its cause. Do not bypass checks, increase test timeouts without a reproduced cause, or repeatedly rerun unchanged failures. Close superseded or deliberately deferred proposals only after confirming their changes are accounted for; remove only bot-owned branches without additional commits.

## Pull requests

Explain the concrete problem, changed behavior, compatibility implications and relevant test results. Include a short Chinese changelog entry for user-visible changes. UI checks should identify synthetic data, viewport and theme; do not include production screenshots.

Run `gitleaks git --config .gitleaks.toml` before publishing a branch. Allowlisting is limited to exact, public synthetic fixture values. Do not add broad exclusions.

Report suspected security vulnerabilities privately through the repository's GitHub Security Advisory feature. Do not put credentials or exploit details in a public issue.
