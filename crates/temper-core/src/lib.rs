//! temper-core — the library behind the `fleet` CLI.
//!
//! `fleet` converges a machine to a declared spec kept in a folder of
//! human-readable files (git / Nextcloud / USB — the tool doesn't manage the
//! folder, only reads it). Every capability lives here as a reusable, typed
//! function; the CLI is a thin `--json`-emitting layer with progress bars and
//! an interactive plan/confirm gate on top.
//!
//! Design rules that hold across the crate (see ../../PRINCIPLES.md):
//! - Closed primitive set; open app library. New primitive = big deal.
//! - Every mutation is plan → apply → drift → undo, and journaled.
//! - Packages compose at declaration time, converge as one whole-machine call.
//! - Gate config on reality (a presence probe), not intent.
//! - Nothing is enforced without a drift story.
//!
//! Status: scaffold. Modules are declared to fix the architecture in code;
//! their bodies are filled in incrementally as the ReinstallScripts migration
//! proceeds (see ../../README.md build sequence).

pub mod discovery; // find the temper-home folder across any delivery backend
pub mod manifest; // parse temper.toml + apps/*.toml (machines, apps, ignore)
pub mod packages; // package model: parse, effective set, drift (pure)
pub mod machine; // machine identity (hostname) + role derivation
pub mod primitives; // copy / block / setkey / profile / exec
pub mod providers; // brew / flatpak / mas / gext / rpm-ostree: converge + probe
pub mod drift; // assertions, exec-hooks, status-only reporting
pub mod plan; // build a plan, then apply it (dry-run first)
pub mod reconcile; // interactive spec←machine capture (adds/drops to the Brewfile)
pub mod dconf; // whole-desktop dconf snapshots: backup (filtered) + restore
pub mod probe; // presence probes for when/needs step gating
pub mod journal; // content-addressed, after-hash-guarded undo (amdl model)
pub mod ui; // stdout/stderr discipline + progress bars
