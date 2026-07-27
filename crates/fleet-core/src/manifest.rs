//! Parse and validate the manifest: `fleet.toml` (machines, per-machine app
//! composition, loose `packages`, `[ignore]`) and `apps/<name>.toml` bundles
//! (packages, `when` probe, ordered steps, `[[assert]]`). See ../../SPEC.md.
//!
//! The effective package set for a machine =
//!   union(composed apps' packages) + machine loose packages − [ignore].

// TODO: serde structs for Machine, Bundle, Step, Assert, Probe; validate that
// every `apps` reference resolves and every step names exactly one primitive.
