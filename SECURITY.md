# Security policy

Report vulnerabilities privately through the repository's GitHub Security Advisory workflow. Do not include credentials, production ledger data, or session files in public issues. Rotate exposed credentials before publishing a report.

The service uses a column-restricted, read-only source database role. SSO and all writes still verify the upstream identity. Deploy behind the supplied same-origin TLS proxy and keep the data volume private. See [deployment](docs/deploy.md) for limits and [upgrade](docs/upgrade.md) for offline key rotation and recovery.

CI checks Rust advisories, dependency licenses, frontend advisories and the compiled Cargo feature graph (rejecting any `rsa` or `sqlx-mysql` dependency; the application does not use MySQL). All advisories fail CI without active waivers.

The application is MIT-licensed. Dependencies retain their own licenses; the Fernet implementation is MPL-2.0 and its source is available from the pinned Cargo dependency. The runtime release includes an SPDX SBOM. Image signatures and SBOM attestations use GitHub Actions OIDC with Sigstore; verify the tag workflow identity and image digest, not only the tag name.
