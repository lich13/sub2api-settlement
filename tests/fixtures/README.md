# Frozen compatibility fixtures

These files contain synthetic users, requests, account names, money, session
keys and tokens. No production database, session or upstream was used.

- `http/upstream/source.initial.json` provides the synthetic source snapshot
  for isolated HTTP, reminder and operations tests.
- `ledger/legacy-written.sql` and `ledger/legacy-written-source.json` provide a
  restorable dump of a synthetic legacy ledger and its matching source snapshot
  for migration, backup and restart continuation tests.
- `ledger/seed.sql` provides minimal synthetic seed rows for release packaging
  and migration checks.
- `session/legacy-restart.json` contains public synthetic cryptographic vectors
  for validating Fernet session rotation and restart continuation.

Do not regenerate these files during normal tests or change expected financial
results to make a new implementation pass. The seven files under `../golden/`
remain byte-for-byte unchanged.
