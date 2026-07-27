//! Drift is a subsystem, not a diff. It evaluates, per machine:
//!
//! - package-set drift (dependency-aware; from `providers`),
//! - managed-file drift (byte, or semantic for dynamic/templated values),
//! - key drift (`setkey` reads the key back),
//! - `[[assert]]`: absent, mode, owner, contains-line, not-member,
//!   executable-resolves, json-semantic, shell,
//! - exec drift-hooks (a step's companion `check` script),
//! - status-only items fleet reports but cannot repair (image origin).
//!
//! Reports per-app: present & drifted vs absent & N/A (skipped). Read-only.

// TODO: enum Finding { InSync, Drifted{..}, NotApplicable, StatusOnly{..} }.
