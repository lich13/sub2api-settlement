# Security policy

Report vulnerabilities privately through the repository's GitHub Security Advisory workflow. Do not include credentials, production ledger data, or session files in public issues. Rotate exposed credentials before publishing a report.

The service uses a column-restricted, read-only source database role. SSO and all writes still verify the upstream identity. Deploy behind the supplied same-origin TLS proxy and keep the data volume private. See [deployment](docs/deploy.md) for limits and [upgrade](docs/upgrade.md) for offline key rotation and recovery.

CI checks Rust advisories, dependency licenses and frontend advisories. `RUSTSEC-2023-0071` is excluded only because the affected `rsa` crate belongs to SQLx's inactive optional MySQL dependency in the lockfile; a separate feature-graph check rejects any compiled `rsa` or `sqlx-mysql` dependency. The application does not use MySQL. Other advisories fail CI.

The application is MIT-licensed. Dependencies retain their own licenses; the Fernet implementation is MPL-2.0 and its source is available from the pinned Cargo dependency. The runtime release includes an SPDX SBOM. Image signatures and SBOM attestations use GitHub Actions OIDC with Sigstore; verify the tag workflow identity and image digest, not only the tag name.
