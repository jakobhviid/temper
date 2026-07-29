//! Machine identity and role.
//!
//! Identity is (name, os, role). This machine is resolved by hostname (RIS's
//! `current_machine_name`: `hostname`, lowercased, domain suffix stripped),
//! with an explicit override and a single-machine fallback.
//!
//! Role (desktop | server) is read from the manifest and validated at load; it
//! gates OS/role-scoped bundles and steps.

use anyhow::{anyhow, bail, Result};

use crate::manifest::{Machine, TemperToml};

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
pub fn resolve(ft: &TemperToml, explicit: Option<&str>) -> Result<Machine> {
    if ft.machine.is_empty() {
        bail!("no [[machine]] entries in temper.toml");
    }
    if let Some(name) = explicit {
        return ft
            .machine
            .iter()
            .find(|m| m.name == name)
            .cloned()
            .ok_or_else(|| anyhow!("no machine named '{name}' in temper.toml"))
            .and_then(checked);
    }
    if let Some(h) = hostname() {
        if let Some(m) = ft.machine.iter().find(|m| m.name == h) {
            return checked(m.clone());
        }
    }
    if ft.machine.len() == 1 {
        return checked(ft.machine[0].clone());
    }
    let names: Vec<&str> = ft.machine.iter().map(|m| m.name.as_str()).collect();
    bail!(
        "could not resolve this machine by hostname; pass a name (known: {})",
        names.join(", ")
    )
}

/// Reject an unknown `os` (a typo like "Linux" would otherwise silently skip
/// every os-gated step). Also validates the role if declared.
fn checked(m: Machine) -> Result<Machine> {
    if !matches!(m.os.as_str(), "mac" | "linux") {
        bail!(
            "machine '{}' has unknown os '{}' (expected \"mac\" or \"linux\")",
            m.name,
            m.os
        );
    }
    if let Some(role) = &m.role {
        if !matches!(role.as_str(), "desktop" | "server") {
            bail!(
                "machine '{}' has unknown role '{}' (expected \"desktop\" or \"server\")",
                m.name,
                role
            );
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(os: &str, role: Option<&str>) -> Machine {
        Machine {
            name: "x".into(),
            os: os.into(),
            role: role.map(String::from),
            apps: vec![],
            packages: vec![],
            brewfile: None,
            vars: Default::default(),
            dconf: vec![],
            git: None,
        }
    }

    #[test]
    fn checked_rejects_unknown_os_and_role() {
        // Valid combinations pass through.
        assert!(checked(m("mac", Some("desktop"))).is_ok());
        assert!(checked(m("linux", Some("server"))).is_ok());
        assert!(checked(m("linux", None)).is_ok());
        // A case typo ("Linux") would otherwise silently skip EVERY os-gated
        // step — it must error instead.
        assert!(checked(m("Linux", None)).is_err());
        assert!(checked(m("windows", None)).is_err());
        // An unknown role errors too.
        assert!(checked(m("linux", Some("laptop"))).is_err());
    }
}
