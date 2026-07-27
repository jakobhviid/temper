//! The closed set of config primitives (app-scope). Each implements the shared
//! plan → apply → drift → undo contract.
//!
//! - `copy`   — deploy a file/dir → target(s). Modes: verbatim, template
//!              (declared vars + `{{ … }}` apply-time probes), seed (create-once,
//!              hands-off, excluded from drift). Fields: `to`, `mode` (perms).
//! - `block`  — ensure a marker-delimited block/line is present in a user file
//!              (the grove-`setup` pattern: SSH `Include`, zshrc `source`).
//! - `setkey` — set key(s) in a structured store, preserving siblings. Backends:
//!              dconf, macOS defaults, ini/.desktop, json, toml. Supports
//!              list-append (array-union) and dynamic values.
//! - `profile`— install a macOS .mobileconfig (weak contract: GUI apply,
//!              plist-subset drift).
//! - `exec`   — run a user script. Declares privilege (sudo), secrets/env, and
//!              an optional `check` drift-hook. The only escape hatch.

// TODO: trait Primitive { fn plan; fn apply; fn drift; fn undo_entry; }
