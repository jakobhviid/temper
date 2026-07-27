//! The closed set of config primitives (app-scope). Each implements the shared
//! plan → apply → drift → undo contract. Live now: `copy` (verbatim, template,
//! seed, mode). Next: `block`, `setkey`, `profile`, `exec`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::journal::Journal;

/// The sync state of a deployed file relative to its (rendered) source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// Target absent.
    Missing,
    /// Target matches source.
    InSync,
    /// Target present but differs.
    Drifted,
}

impl FileState {
    pub fn label(self) -> &'static str {
        match self {
            FileState::Missing => "missing",
            FileState::InSync => "in sync",
            FileState::Drifted => "drifted",
        }
    }

    pub fn is_ok(self) -> bool {
        matches!(self, FileState::InSync)
    }
}

/// Options for a `copy` step.
pub struct CopyOpts<'a> {
    /// Render `{{ … }}` before deploying.
    pub template: bool,
    /// Create-once, then leave alone; excluded from drift.
    pub seed: bool,
    /// Octal mode to enforce on the target (e.g. "0600").
    pub mode: Option<&'a str>,
    /// Declared template variables (for `{{ var "…" }}`).
    pub vars: &'a BTreeMap<String, String>,
}

/// The bytes to deploy: raw, or the rendered template.
fn source_bytes(src: &Path, opts: &CopyOpts) -> Result<Vec<u8>> {
    let raw = fs::read(src).with_context(|| format!("reading source {}", src.display()))?;
    if !opts.template {
        return Ok(raw);
    }
    let text = String::from_utf8(raw)
        .with_context(|| format!("template source {} is not UTF-8", src.display()))?;
    Ok(render(&text, opts.vars)?.into_bytes())
}

/// `copy` drift. Seed steps are excluded (present → in sync, absent → missing so
/// install can create them; content is never compared).
pub fn copy_state(src: &Path, target: &Path, opts: &CopyOpts) -> Result<FileState> {
    if opts.seed {
        return Ok(if target.exists() {
            FileState::InSync
        } else {
            FileState::Missing
        });
    }
    let want = source_bytes(src, opts)?;
    if !target.exists() {
        return Ok(FileState::Missing);
    }
    let have = fs::read(target).with_context(|| format!("reading {}", target.display()))?;
    Ok(if have == want {
        FileState::InSync
    } else {
        FileState::Drifted
    })
}

/// `copy` apply. Returns whether the file's content changed. A seed target that
/// already exists is left untouched. Mode is (idempotently) enforced regardless.
pub fn copy_apply(
    src: &Path,
    target: &Path,
    opts: &CopyOpts,
    journal: &mut Journal,
) -> Result<bool> {
    if opts.seed && target.exists() {
        return Ok(false);
    }
    let want = source_bytes(src, opts)?;
    let before = if target.exists() {
        Some(fs::read(target).with_context(|| format!("reading {}", target.display()))?)
    } else {
        None
    };

    if before.as_deref() == Some(want.as_slice()) {
        apply_mode(target, opts.mode)?;
        return Ok(false);
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    journal.record_write(target, before.as_deref(), &want)?;
    fs::write(target, &want).with_context(|| format!("writing {}", target.display()))?;
    apply_mode(target, opts.mode)?;
    Ok(true)
}

/// Enforce an octal file mode on Unix; a no-op elsewhere.
fn apply_mode(target: &Path, mode: Option<&str>) -> Result<()> {
    let Some(mode) = mode else { return Ok(()) };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bits = u32::from_str_radix(mode.strip_prefix("0o").unwrap_or(mode), 8)
            .with_context(|| format!("invalid octal mode {mode:?}"))?;
        fs::set_permissions(target, fs::Permissions::from_mode(bits))
            .with_context(|| format!("chmod {} {}", mode, target.display()))?;
    }
    #[cfg(not(unix))]
    let _ = target;
    Ok(())
}

// --- minimal templating: {{ func "arg" }} -------------------------------------

fn render(input: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| anyhow!("unterminated `{{{{` in template"))?;
        out.push_str(&eval_expr(after[..end].trim(), vars)?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn eval_expr(expr: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    let (func, arg) = split_call(expr)?;
    match func {
        "which" => which(&arg)
            .map(|p| p.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("which: `{arg}` not found on PATH")),
        "env" => std::env::var(&arg).map_err(|_| anyhow!("env: `{arg}` is not set")),
        "var" => vars
            .get(&arg)
            .cloned()
            .ok_or_else(|| anyhow!("var: `{arg}` is not declared in [vars]")),
        other => bail!("unknown template function `{other}`"),
    }
}

/// Parse `func "arg"` into (func, arg).
fn split_call(expr: &str) -> Result<(&str, String)> {
    let (func, tail) = expr
        .split_once(char::is_whitespace)
        .ok_or_else(|| anyhow!("template expression `{expr}` takes an argument"))?;
    let tail = tail.trim();
    let arg = tail
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .ok_or_else(|| anyhow!("template argument in `{expr}` must be quoted"))?;
    Ok((func, arg.to_string()))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|cand| cand.is_file())
}
