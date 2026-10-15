# Frozen compatibility fixtures

These files were captured once from the legacy implementation identified in
`manifest.json`. They contain fictional users, requests, account names, money,
session keys and tokens. No production database, session or upstream was used.

- `http/workflows/` records requests, responses, upstream calls and database
  observations, including restart, migration and concurrent outcomes.
- `ledger/legacy-written.sql` is a restorable dump of a fictional ledger
  written by the old service. It is tested with its matching synthetic source.
- `ledger/workflows/` holds normalized comparison snapshots, including runtime
  aliases and decoded JSON scalar values. They are comparison inputs, not
  restorable database dumps. `ledger/provenance.json` records the addition of
  missing DDL delimiters and the before/after hashes; the captured archive
  retains the originals.
- `session/` contains public synthetic cryptographic vectors for validating
  retained sessions. These keys must never be used outside isolated tests.

The Rust tests start only loopback services and temporary ledgers. They replace
runtime-generated identifiers, clocks, origins and ciphertext with registered
aliases before comparing observations. Amounts, allocation ownership, versions,
canonical digests, permission errors and durable accounting rows remain exact.
Asynchronous deletion checks wait for the recorded terminal state with a bounded
timeout; intermediate states do not become alternative accepted results.

Do not regenerate these files during normal tests or change expected financial
results to make a new implementation pass. New behavior belongs in separate
tests. The seven files under `../golden/` remain byte-for-byte unchanged.
