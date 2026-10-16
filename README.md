# Sub2API Settlement

[简体中文](README.zh-CN.md)

A self-hosted ledger for shared Sub2API account costs, fixed expenses and offline payments. A Rust service and React frontend run under `/settlement/`, with same-origin sign-in from Sub2API.

Allocate costs using original consumption or matched upstream charges, renew account cycles, and settle reciprocal debts as a single net payment. Amounts are stored in integer cents; frozen settlement records remain unchanged.

![Settlement center with synthetic demonstration data](docs/assets/demo.jpg)

## Quick start

Use Rust 1.99.0, Node 24.21.0 and pnpm 12.11.2, as pinned in this repository. To try the application with a temporary, synthetic ledger:

```sh
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend build
cargo run --locked -p settlement-tools -- demo --netting-fixtures
```

Open the loopback URL printed by the command. The demo includes three fictional users and never connects to a production database. Stop it with Ctrl+C. Demo and load tools are excluded from the runtime image.

For a deployment, copy `.env.example` to `.env`, provide a dedicated read-only Sub2API PostgreSQL role, prepare the persistent data directory, and configure the same-origin Nginx route. Follow the [deployment guide](docs/deploy.md) before starting `docker compose up -d --build`.

Runtime variables, defaults and test-only settings are listed in the deployment guide. Keep credentials and ledger files outside Git.

- [Settlement rules](docs/rules.md) · [Deployment and Sub2API schema compatibility](docs/deploy.md)
- [Upgrade and rollback](docs/upgrade.md) · [Contributing](CONTRIBUTING.md)
- [Changelog](CHANGELOG.md) · [Release process](RELEASING.zh-CN.md) · [MIT license](LICENSE)

Operational guides and release notes are maintained in Chinese.
