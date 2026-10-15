# Reproducible Rust load checks

The harness uses only a loopback Rust service, a synthetic upstream and a temporary ledger. It refuses inherited production configuration. No external system is contacted.

```sh
cargo run --locked -p settlement-tools -- load --self-check
cargo run --locked --release -p settlement-tools -- load --usage 150000 --csv-rows 50000 --samples 10
cargo run --locked --release -p settlement-tools -- load --usage 1000000 --csv-rows 50000 --samples 10
cargo run --locked --release -p settlement-tools -- load --usage 1000000 --csv-rows 50000 --duration 3600 --workers 6
```

The generator deterministically creates 12 accounts in four trees, 64 users and request-ID CSV matches plus unmatched rows. Output includes input hashes, platform, P50/P95 using linear interpolation, completed operations per second, memory scope and SQLite integrity checks. Compare runs only on the same otherwise idle host with identical input hashes and build profile.

Memory is measured for the service and in-process synthetic harness together (Linux high-water RSS; sampled RSS elsewhere). This is a different scope from the historical service-only measurement, and must not be presented as a directly comparable speed or memory improvement. The small self-check uses two concurrent workers, checks repeated writes remain idempotent, and also verifies a source outage and a graceful retained-ledger restart. It does not replace a process-kill test or a 60-minute run. Long runs are opt-in.

`results-v0.9.0.json` is a frozen historical report. It is retained for provenance and does not describe the current release's measured performance.
