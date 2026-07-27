//! Machine identity and role.
//!
//! Identity is (name, os, role). This machine is resolved by hostname (RIS's
//! `current_machine_name`: `hostname`, lowercased, domain suffix stripped),
//! with an explicit override and a single-machine fallback.
//!
//! Role (desktop | server) will be DERIVED from a `gnome-shell` probe rather
//! than trusted from the manifest (a server can't look like a desktop, so the
//! misdetection failure mode is safe). Not needed for the copy vertical.

use anyhow::{anyhow, bail, Result};

use crate::manifest::{FleetToml, Machine};

/// This build's OS in manifest terms.
pub fn current_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

/// Short hostname, lowercased, domain suffix stripped (best-effort).
pub fn hostname() -> Option<String> {
    let out = std::process::Command::new("hostname").output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let short = s.trim().to_lowercase();
    let short = short.split('.').next().unwrap_or(&short).to_string();
    (!short.is_empty()).then_some(short)
}

/// Resolve which machine we are: explicit name → hostname match → the sole
/// machine if there's only one → error listing the known names.
pub fn resolve(ft: &FleetToml, explicit: Option<&str>) -> Result<Machine> {
    if ft.machine.is_empty() {
        bail!("no [[machine]] entries in fleet.toml");
    }
    if let Some(name) = explicit {
        return ft
            .machine
            .iter()
            .find(|m| m.name == name)
            .cloned()
            .ok_or_else(|| anyhow!("no machine named '{name}' in fleet.toml"));
    }
    if let Some(h) = hostname() {
        if let Some(m) = ft.machine.iter().find(|m| m.name == h) {
            return Ok(m.clone());
        }
    }
    if ft.machine.len() == 1 {
        return Ok(ft.machine[0].clone());
    }
    let names: Vec<&str> = ft.machine.iter().map(|m| m.name.as_str()).collect();
    bail!(
        "could not resolve this machine by hostname; pass a name (known: {})",
        names.join(", ")
    )
}
