//! Machine identity and role.
//!
//! Identity is (name, os, role), resolved by hostname at runtime (RIS's
//! `current_machine_name`: `hostname -s`, lowercased, `.local` stripped).
//!
//! Role (desktop | server) is DERIVED from a `gnome-shell` presence probe, not
//! trusted from the manifest — a server can't look like a desktop, so the
//! misdetection failure mode is safe (RIS's `is_desktop`). A declared role is
//! cross-checked against the probe, never blindly obeyed.

// TODO: resolve_machine(), current_os(), derive_role().
