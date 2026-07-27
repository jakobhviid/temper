//! Build a plan (the ordered set of steps that would run for a machine + flow),
//! then apply it. Dry-run first, then an interactive "show plan → confirm →
//! apply" gate on a terminal (RIS's Plan / `Proceed? [y/N]`).
//!
//! Two automated flows:
//! - install: full converge (add missing, apply everything, one-time setup).
//! - update:  upgrade + re-apply `always` + honor `ensure` (install-if-missing
//!            for an allowlist). Does NOT add newly-declared apps wholesale.

// TODO: Plan { machine, flow, steps }; apply() journals every mutation.
