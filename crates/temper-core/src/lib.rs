//! temper-core — the library behind the `temper` CLI.
//!
//! `temper` converges a machine to a declared spec kept in a folder of
//! human-readable files (git / Nextcloud / USB — the tool doesn't manage the
//! folder, only reads it). Every capability lives here as a reusable, typed
//! function; the CLI is a thin `--json`-emitting layer with an interactive
//! plan/confirm gate on top.
//!
//! Design rules that hold across the crate (see ../../PRINCIPLES.md):
//! - Three sets: primitives are closed (new one = big deal), app-bundles are
//!   free config, providers are open behind the eleven-column interface.
//! - Scope decides the verb set: a fleet declaration is drift + install; a
//!   machine declaration adds prune + reconcile. `prune` enacts removal at both.
//! - Capability is per cell, and absence you could not observe is not evidence.
//! - Every mutation is plan → apply → drift → undo, and journaled.
//! - Packages compose at declaration time, converge as one whole-machine call.
//! - Gate config on reality (a presence probe), not intent.
//! - Nothing is enforced without a drift story, and nothing is reported without
//!   a resolution story.
//!
//! The ReinstallScripts migration this crate grew out of is complete: every
//! module below is implemented and exercised end-to-end (see ../../README.md).

pub mod dconf;
pub mod interface; // whole-desktop dconf snapshots: capture (filtered) + drift + restore
pub mod discovery; // find the temper-home folder across any delivery backend
pub mod drift; // assertions, exec-hooks, status-only reporting
pub mod eq_import; // folder-authoring: pull calibrated speaker profiles into the folder
pub mod git; // optional convenience: persist temper's own writes to a git home
pub mod journal; // content-addressed, after-hash-guarded undo (amdl model)
mod jsonc; // comment-preserving JSONC edits for the setkey(json) backend
pub mod machine; // machine identity (hostname) + role derivation
pub mod manifest; // parse temper.toml + apps/*.toml (machines, apps, ignore)
pub mod packages; // package model: parse, effective set, drift (pure)
pub mod plan; // build a plan, then apply it (dry-run first)
pub mod primitives; // copy / block / setkey / profile / exec
pub mod probe; // presence probes for when/needs step gating
pub mod providers; // brew / flatpak / mas / gext / rpm-ostree: converge + probe
pub mod reconcile; // interactive spec←machine capture (adds/drops to the Brewfile)
pub mod settings; // `temper configure`: validated scalar settings in temper.toml
pub mod sudo; // hold a granted sudo timestamp open so a long run prompts once
pub mod ui; // stdout/stderr discipline + progress bars
