# Local-startup credential hardening: source-only handoff

Based on `92fae64`, on branch `b22-winstart-hardening`. The original twelve-file
uncommitted handoff is retained, including the parent's NUL-path, cancellation
and counted Stop/retry corrections. This is not a rebuilt Windows preview.

## Design and production paths

- `src-tauri/src/server_config.rs` contains the actual `ClientConfig` and
  `SavedServer` types and server-list write helpers, not test-only replicas.
  `revision: u32` has `serde(default)`: a valid existing profile loads at zero
  without losing its servers, credentials, active selection or route fields.
- `servers_get` returns the redacted snapshot with its revision. `servers_save`
  checks the revision before merging stored secrets. A mismatch rejects the
  whole save without changing memory or disk. It does not retry or rebase.
  Every accepted UI save and native local/pair/selection/key-pinning change
  uses a checked revision increment. Exhaustion refuses instead of wrapping.
- Credential/list transactions clone the current config, persist and verify
  the exact next bytes, then publish under the existing config mutex. Failed
  persistence does not publish an in-memory replacement or retire old logins.
  `AppState::save` already holds its guard throughout ordinary metadata writes;
  that method, route-follow and last-endpoint persistence were not refactored.
  Routes still use `route_rev`; last-endpoint metadata comes from the stored copy.
- `src-tauri/src/credential_write.rs` owns the one shared async mutex in
  `AppState`. Its factory begins only after acquiring ownership. Local startup,
  ordinary pairing and link pairing hold it from before issuance through save,
  retirement/connection handoff. Local key pinning uses it too. No
  `parking_lot` config guard spans an await. Pairing's existing `RetireLogin`
  message retires the particular old login once connected; it is not a new
  synchronous network-cleanup guarantee.
- Startup's `OneJob` now owns issuance, persistence, CLI retirement and connect,
  not just issuance. Stop stops watching, not the job. The CLI issues with
  `device-token --keep-old`; only after a verified save does the app run
  `device-token-retire --keep <public device id>`. A cleanup failure leaves the
  new login saved and may leave old logins valid. No bearer token enters argv.
- `src-tauri/src/profile_owner.rs` owns `<app profile>/profile.lock`. Desktop
  setup acquires it before reading `servers.json` and retains it as managed
  state for the app lifetime. Unix uses nonblocking flock, retries EINTR and
  rejects all other errors (including unsupported locking); its descriptor is
  close-on-exec and its lock file is not opened through a symlink. Windows
  opens without sharing. A second owner or an unexpected error refuses setup.
  App-data-path resolution no longer falls back silently to the current folder.
- This is deliberately not a mobile process lock. Android
  `src-tauri/src/notify/pull.rs` loads the app library independently, reads
  `servers.json`/`notifications.json`, and writes `notify-pull.json`. The
  background architecture and its separate files are unchanged.
- UI writes preserve the snapshot revision. The unnecessary post-pair and
  pre-connect whole-list saves in `TauriTransport` are removed: their native
  commands already save and select. A failed config read is not replaced with
  a fabricated empty list. Stale saves propagate an error without automatic
  retry; a later explicit user action can read current settings anew.
- Inherited server/launcher follow-ups remain: Unix server-lock EINTR retry,
  explicit unsupported-filesystem warning, unexpected-error refusal; Windows
  launcher claim refusal and log output rather than `Claim::Unknown` fallback.
  Neither of those locks is the desktop profile-owner lock.

## Regression fixtures

This standalone fixture includes the production modules by path. It adds the
actual client dependency for `KnownRoute` and endpoint/key helpers; it does not
substitute string configs for credential/list race tests.

**New/strengthened Rust tests, authored but NOT compiled or run in this handoff:**

- `server_config::tests::stale_old_id_and_missing_new_entry_cannot_erase_a_verified_local_login`
- `server_config::tests::same_key_pair_replacement_and_native_selection_invalidate_ui_snapshots`
- `server_config::tests::current_ui_change_preserves_secrets_and_bumps_the_persisted_revision`
- `server_config::tests::old_profiles_load_at_revision_zero_without_resetting_their_login`
- `server_config::tests::failed_native_save_never_publishes_a_replacement_or_new_revision`
- `server_config::tests::revision_exhaustion_refuses_instead_of_reusing_a_stale_generation`
- `tests::stop_and_retry_during_retirement_join_one_saved_login_job`: now uses
  real server configs, verified files and stale CAS during paused retirement.
- `tests::native_pair_and_local_start_serialize_before_issuance_through_handoff_in_both_orders`:
  deterministic future polling, real local/pair save helpers and file read-back.
- `profile_owner::tests::second_profile_owner_cannot_load_or_write_until_first_closes`
- `profile_owner::tests::separate_process_profile_owner_is_refused_then_allowed_after_close`
  (uses the isolated `profile_owner_child` test subprocess).
- `profile_owner::tests::profile_open_and_lock_errors_never_fall_back_to_an_unowned_profile`
- Unix-only `profile_flock_retries_interrupts_but_never_uses_an_unsupported_fallback`
  and `profile_claim_does_not_follow_a_planted_lock_symlink`.

The original CLI-run, OneJob, local-login, private-file and Unix data-lock
module tests remain included. Original CLI integration tests live in
`crates/cli/tests/device_token.rs`; original store-retirement tests live in
`crates/core/src/store.rs`; original launcher tests live in
`crates/service/src/main.rs`. The Windows-only private-file NUL-before-FFI test
is preserved. All those Rust tests also remain unrun in this source-only turn.

**Source checks actually run, 2026-10-09:**

- Direct `rustfmt --edition 2024` plus `--check` for all twelve changed/new Rust
  files: passed. No Cargo command was used.
- `git diff --check`: passed.
- `node --check apps/tessera/tests/startup/ui-config.test.mjs`: passed.
- `ui-config.test.mjs`: **8/8 passed**. It transpiles the checked-out production
  UI writers into memory and uses a fake invoke/backend. It checks revision
  preservation/no stale retry for Save, Remove, Forget and device-name writes;
  failed reads cannot lead to a save; pair/link-pair have no follow-up
  whole-list save; native selection has no snapshot save; the native save
  bridge propagates rejection exactly once. No browser or native app is run.
  The worktree has no node_modules; the check used the existing canonical
  checkout's TypeScript 5.9.3 via `TYPESCRIPT_PATH`, without installing anything.

## Deferred verification and limits

No Cargo resolution, Rust compile/test, Clippy, full UI typecheck/build, native
runtime, Windows acceptance, machine command, model call or helper was run.
Do not describe this pin as compiled, runtime-approved or ready to deploy.

When the parent releases the build lanes, explicitly run the standalone tests
(they are not root-workspace members), the CLI/store/service tests, full native
app compilation/Clippy for desktop targets and the UI typecheck/build. Example
commands for the later authorized lane, NOT executed here:

```sh
cargo test --manifest-path apps/tessera/tests/startup/Cargo.toml
cargo test -p tessera --test device_token
cargo test -p tessera-core local_login_retirement
cargo test -p tessera-core unknown_or_machine_kept_login
cargo test -p tessera-core retirement_failure_rolls_back
cargo test -p tessera-service
# Windows/native app check, using the parent's established isolated build lane.
```

Issuance/network/CLI cleanup and connection handoff in the race fixture are
controlled futures. It tests the actual sequencing and persistence helpers,
not a real server or Tauri command round trip. The profile subprocess fixture
is authored, not executed. Full app lifetime, Windows sharing, Windows durable
replace, Android background notifications and UI rendering still need native
acceptance. Existing valid profile compatibility is covered by an authored
serde fixture, not a tested upgrade of the user's profile.

As before, Stop/retry and close/reopen acceptance must demonstrate one server
and launcher and a working saved login. Add a remote-pair overlap and a second
same-profile desktop launch. Distinct profiles should remain independent.
Locks coordinate updated app processes; an older binary or an external program
that ignores the profile lock is outside that coordination. Unsupported profile
locking fails closed, whereas the inherited server lock has its explicit weaker
unsupported-filesystem path. No installation, preview binary, user data reset,
production change, merge or deployment is part of this source pin.
