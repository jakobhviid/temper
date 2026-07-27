//! Locate the fleet-home folder — the directory holding `fleet.toml`.
//!
//! Order (reused from dotsync's `discovery.rs`): `$FLEET_DIR` → a saved pointer
//! in per-machine config → auto-scan common locations (a git checkout, a cloud
//! folder, a mount) → prompt on a terminal. First run offers setup.
//!
//! fleet is delivery-agnostic: it never runs git or a sync client — it only
//! needs a path that contains a manifest.

// TODO: port dotsync::discovery, dropping the cloud-specific "must not be a git
// repo" guard (a fleet-home git repo is fine — it holds specs, not secrets).
