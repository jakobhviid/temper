//! Presence probes for `when`/`needs` step gating (Principle #5: gate config on
//! reality, not intent). Each probe shells out best-effort; a probe with no
//! field set never passes (fails safe). All read-only.

use std::path::Path;
use std::process::Command;

use crate::manifest::{expand_tilde, Probe};
use crate::primitives::which;

/// Whether a command exits 0 (stdout/stderr captured, never leaked).
fn succeeds(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Does this probe pass on this machine right now?
pub fn passes(home: &Path, p: &Probe) -> bool {
    if let Some(b) = &p.binary {
        return which(b).is_some();
    }
    if let Some(path) = &p.path {
        return expand_tilde(path).exists();
    }
    if let Some(x) = &p.brew {
        return which("brew").is_some() && succeeds("brew", &["list", "--formula", x]);
    }
    if let Some(x) = &p.cask {
        return which("brew").is_some() && succeeds("brew", &["list", "--cask", x]);
    }
    if let Some(x) = &p.flatpak {
        return which("flatpak").is_some() && succeeds("flatpak", &["info", x]);
    }
    if let Some(id) = &p.mas {
        if which("mas").is_none() {
            return false;
        }
        return Command::new("mas")
            .arg("list")
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|l| l.split_whitespace().next() == Some(id.as_str()))
            })
            .unwrap_or(false);
    }
    if let Some(uuid) = &p.gext {
        return which("gnome-extensions").is_some() && succeeds("gnome-extensions", &["info", uuid]);
    }
    if let Some(x) = &p.rpm {
        return which("rpm").is_some() && succeeds("rpm", &["-q", x]);
    }
    if let Some(script) = &p.exec {
        return Command::new("sh")
            .arg(home.join(script))
            .current_dir(home)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
    }
    false
}

/// A short human label for the probe (for skip/error messages).
pub fn describe(p: &Probe) -> String {
    if let Some(b) = &p.binary {
        format!("binary `{b}`")
    } else if let Some(x) = &p.path {
        format!("path `{x}`")
    } else if let Some(x) = &p.brew {
        format!("brew `{x}`")
    } else if let Some(x) = &p.cask {
        format!("cask `{x}`")
    } else if let Some(x) = &p.flatpak {
        format!("flatpak `{x}`")
    } else if let Some(x) = &p.mas {
        format!("mas `{x}`")
    } else if let Some(x) = &p.gext {
        format!("gext `{x}`")
    } else if let Some(x) = &p.rpm {
        format!("rpm `{x}`")
    } else if let Some(x) = &p.exec {
        format!("exec `{x}`")
    } else {
        "empty probe".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn empty() -> Probe {
        Probe {
            binary: None,
            path: None,
            brew: None,
            cask: None,
            flatpak: None,
            mas: None,
            gext: None,
            rpm: None,
            exec: None,
        }
    }

    #[test]
    fn binary_probe() {
        let mut p = empty();
        p.binary = Some("sh".into()); // sh is always present
        assert!(passes(&PathBuf::from("/"), &p));
        p.binary = Some("definitely-not-a-real-binary-xyz".into());
        assert!(!passes(&PathBuf::from("/"), &p));
    }

    #[test]
    fn path_probe() {
        let mut p = empty();
        p.path = Some("/".into());
        assert!(passes(&PathBuf::from("/"), &p));
        p.path = Some("/no/such/path/xyz".into());
        assert!(!passes(&PathBuf::from("/"), &p));
    }

    #[test]
    fn empty_probe_fails_safe() {
        assert!(!passes(&PathBuf::from("/"), &empty()));
        assert_eq!(describe(&empty()), "empty probe");
    }
}
