//! Drift-only assertions — checks that aren't a converge action. Live:
//! `absent`, `contains_line`, `mode`, `executable_resolves`. (`not_member`,
//! `shell`, `json_semantic` land with the system-facing slices.)
//!
//! Each returns (ok, human status). The plan layer folds these into the drift
//! report alongside the file/key primitives.

use std::fs;

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
    } else {
        "unknown"
    }
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
    } else {
        String::new()
    }
}

/// Evaluate an assertion: (ok, status message).
pub fn eval(a: &Assert) -> Result<(bool, String)> {
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

    bail!("assertion has no recognized check field")
}
