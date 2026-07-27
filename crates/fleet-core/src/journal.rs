//! Content-addressed, after-hash-guarded undo — lifted from amdl's model.
//!
//! A mutating run writes `runs/<run-id>/manifest.json` + a content-addressed
//! `objects/` store under the XDG state dir. Each entry is a minimal inverse
//! (Create / Restore / RestoreBytes / Removed). `fleet undo` reverts in reverse
//! order, and every revert is guarded by an after-hash check: if the file no
//! longer hashes to what fleet left, the entry is skipped-and-reported, never
//! clobbered. Retention keeps the newest N runs.

// TODO: begin(argv) / created() / replaced() / removed() / edit() / commit();
//       undo(run) with the skip-and-report guard; gc_objects().
