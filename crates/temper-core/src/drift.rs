//! Drift-only assertions — checks that aren't a converge action:
//! `absent`, `contains_line`, `mode`, `executable_resolves`, `not_member`,
//! `shell`, `json_semantic`.
//!
//! Each returns (ok, human status). The plan layer folds these into the drift
//! report alongside the file/key primitives.

use std::fs;
use std::path::Path;

use anyhow::{bail, Result};

use crate::manifest::{expand_tilde, Assert};
use crate::primitives::which;

/// A short label for the kind of check this assertion is.
pub fn kind(a: &Assert) -> &'static str {
    if a.absent.is_some() {
        "absent"
    } else if a.contains_line.is_some() {
        "contains-line"
    } else if a.mode.is_some() {
        "mode"
    } else if a.executable_resolves.is_some() {
        "executable-resolves"
    } else if a.not_member.is_some() {
        "not-member"
    } else if a.shell.is_some() {
        "shell"
    } else if a.json_semantic.is_some() {
        "json-semantic"
    } else {
        "unknown"
    }
}

/// Whether a finding `kind` came from an `[[assert]]`.
///
/// Assertions are **drift-only**: they report a condition, and no verb converges
/// them. Remediation has to know that, or it offers `install` for a staged
/// ostree deployment that only a reboot clears.
pub fn is_assert_kind(kind: &str) -> bool {
    matches!(
        kind,
        "absent"
            | "contains-line"
            | "mode"
            | "executable-resolves"
            | "not-member"
            | "shell"
            | "json-semantic"
    )
}

/// The target the assertion is about (for reporting).
pub fn target(a: &Assert) -> String {
    if let Some(p) = &a.absent {
        p.clone()
    } else if let Some(c) = &a.contains_line {
        c.file.clone()
    } else if let Some(m) = &a.mode {
        m.path.clone()
    } else if let Some(x) = &a.executable_resolves {
        x.clone()
    } else if let Some(g) = &a.not_member {
        format!("group:{}", g.group)
    } else if let Some(s) = &a.shell {
        s.clone()
    } else if let Some(j) = &a.json_semantic {
        j.file.clone()
    } else {
        String::new()
    }
}

/// The current user's group names (`id -Gn`), split on whitespace.
fn user_groups() -> Vec<String> {
    std::process::Command::new("id")
        .arg("-Gn")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// The current user's login shell (dscl on macOS, getent on Linux).
fn login_shell() -> Option<String> {
    let user = std::env::var("USER").ok()?;
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("dscl")
            .args([".", "-read", &format!("/Users/{user}"), "UserShell"])
            .output()
            .ok()?;
        // "UserShell: /bin/zsh"
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .last()
            .map(String::from)
    } else {
        let out = std::process::Command::new("getent")
            .args(["passwd", &user])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .rsplit(':')
            .next()
            .map(String::from)
    }
}

/// The final path component of a shell path. `/bin/zsh`, `/usr/bin/zsh`
/// (usrmerge), and a brew-installed zsh are all the same shell — only the
/// basename matters; comparing raw paths would report perpetual drift on any
/// machine whose login shell lives at a different (equivalent) path.
fn shell_basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// Evaluate an assertion: (ok, status message). `home` resolves reference files.
pub fn eval(home: &Path, a: &Assert) -> Result<(bool, String)> {
    if let Some(path) = &a.absent {
        let p = expand_tilde(path);
        return Ok(if p.exists() {
            (false, "should not exist".into())
        } else {
            (true, "absent".into())
        });
    }

    if let Some(c) = &a.contains_line {
        let p = expand_tilde(&c.file);
        if !p.exists() {
            return Ok((false, "file missing".into()));
        }
        let hay = fs::read_to_string(&p)?;
        let present = hay.lines().any(|l| l.trim() == c.line.trim());
        return Ok(if present {
            (true, "line present".into())
        } else {
            (false, format!("missing line `{}`", c.line))
        });
    }

    if let Some(m) = &a.mode {
        let p = expand_tilde(&m.path);
        if !p.exists() {
            return Ok((false, "path missing".into()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let want = u32::from_str_radix(m.mode.strip_prefix("0o").unwrap_or(&m.mode), 8)?;
            let have = fs::metadata(&p)?.permissions().mode() & 0o777;
            return Ok(if have == want {
                (true, format!("mode {:o}", have))
            } else {
                (false, format!("mode {:o}, want {:o}", have, want))
            });
        }
        #[cfg(not(unix))]
        return Ok((true, "mode check skipped (non-unix)".into()));
    }

    if let Some(cmd) = &a.executable_resolves {
        return Ok(if which(cmd).is_some() {
            (true, "resolves".into())
        } else {
            (false, "not on PATH".into())
        });
    }

    if let Some(g) = &a.not_member {
        let member = user_groups().iter().any(|x| x == &g.group);
        return Ok(if member {
            (false, format!("in group {}", g.group))
        } else {
            (true, "not a member".into())
        });
    }

    if let Some(want) = &a.shell {
        // Match by basename, not full path (see `shell_basename`).
        return Ok(match login_shell() {
            Some(have) if shell_basename(&have) == shell_basename(want) => (true, have),
            Some(have) => (false, format!("shell {have}, want {want}")),
            None => (false, "could not read login shell".into()),
        });
    }

    if let Some(j) = &a.json_semantic {
        let dep = expand_tilde(&j.file);
        let reference = home.join(&j.against);
        if !dep.exists() {
            return Ok((false, "file missing".into()));
        }
        // Tolerant parse: either side may be JSONC (comments / trailing commas).
        let a_val = crate::jsonc::parse_value(&fs::read_to_string(&dep)?)?;
        let b_val = crate::jsonc::parse_value(&fs::read_to_string(&reference)?)?;
        return Ok(if a_val == b_val {
            (true, "matches reference".into())
        } else {
            (false, "differs from reference".into())
        });
    }

    bail!("assertion has no recognized check field")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_matches_by_basename_across_equivalent_paths() {
        // usrmerge / brew put the same shell at different paths — all match.
        assert_eq!(shell_basename("/usr/bin/zsh"), shell_basename("/bin/zsh"));
        assert_eq!(
            shell_basename("/opt/homebrew/bin/zsh"),
            shell_basename("/bin/zsh")
        );
        // a bare name is its own basename
        assert_eq!(shell_basename("zsh"), "zsh");
        // different shells must NOT match — guards against a raw-path regression
        // (which would report perpetual drift or match the wrong shell).
        assert_ne!(shell_basename("/bin/bash"), shell_basename("/bin/zsh"));
    }
}
