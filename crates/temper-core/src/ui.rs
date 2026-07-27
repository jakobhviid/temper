//! Output discipline so `--json` stays pipe-clean: human output → stdout,
//! progress bars + errors → stderr (amdl's `ui.rs` rule). A 3-level verbosity
//! (quiet / normal / verbose) is set once by the CLI and read here.
//!
//! Progress bars are `indicatif` (the amdl look Jakob likes).

// TODO: is_quiet()/is_verbose(); a progress-bar helper for converge + apply.
