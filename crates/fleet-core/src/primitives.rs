//! The closed set of config primitives (app-scope). Each implements the shared
//! plan → apply → drift → undo contract. Live now: `copy` (verbatim, template,
//! seed, mode). Next: `block`, `setkey`, `profile`, `exec`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value as Json;

use crate::journal::Journal;
use crate::manifest::SetKey;

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

pub(crate) fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|cand| cand.is_file())
}

// --- block: ensure a marker-delimited region is present in a user file --------

fn markers(marker: &str) -> (String, String) {
    (
        format!("# >>> fleet:{marker} >>>"),
        format!("# <<< fleet:{marker} <<<"),
    )
}

/// The file content that should result from ensuring `body` sits inside the
/// marker region — replacing an existing region or appending a new one.
fn block_desired(existing: &str, begin: &str, end: &str, body: &str) -> String {
    let region = format!("{begin}\n{}\n{end}", body.trim_end_matches('\n'));
    if let (Some(bs), Some(es)) = (existing.find(begin), existing.find(end)) {
        let region_end = es + end.len();
        let mut out = String::with_capacity(existing.len() + region.len());
        out.push_str(&existing[..bs]);
        out.push_str(&region);
        out.push_str(&existing[region_end..]);
        out
    } else {
        let mut out = existing.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&region);
        out.push('\n');
        out
    }
}

pub fn block_state(body_src: &Path, target: &Path, marker: &str) -> Result<FileState> {
    let body = fs::read_to_string(body_src)
        .with_context(|| format!("reading block source {}", body_src.display()))?;
    if !target.exists() {
        return Ok(FileState::Missing);
    }
    let existing =
        fs::read_to_string(target).with_context(|| format!("reading {}", target.display()))?;
    let (begin, end) = markers(marker);
    let want = block_desired(&existing, &begin, &end, &body);
    Ok(if existing == want {
        FileState::InSync
    } else {
        FileState::Drifted
    })
}

pub fn block_apply(
    body_src: &Path,
    target: &Path,
    marker: &str,
    journal: &mut Journal,
) -> Result<bool> {
    let body = fs::read_to_string(body_src)
        .with_context(|| format!("reading block source {}", body_src.display()))?;
    let before = if target.exists() {
        Some(fs::read(target).with_context(|| format!("reading {}", target.display()))?)
    } else {
        None
    };
    let existing = before
        .as_ref()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    let (begin, end) = markers(marker);
    let want = block_desired(&existing, &begin, &end, &body);
    if before.as_deref() == Some(want.as_bytes()) {
        return Ok(false);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    journal.record_write(target, before.as_deref(), want.as_bytes())?;
    fs::write(target, want.as_bytes()).with_context(|| format!("writing {}", target.display()))?;
    Ok(true)
}

// --- setkey: set a key in a structured file, preserving siblings --------------

fn sk_file(sk: &SetKey) -> Result<PathBuf> {
    let f = sk
        .file
        .as_deref()
        .ok_or_else(|| anyhow!("setkey({}) requires `file`", sk.backend))?;
    Ok(crate::manifest::expand_tilde(f))
}

fn toml_to_json(v: &toml::Value) -> Json {
    match v {
        toml::Value::String(s) => Json::from(s.clone()),
        toml::Value::Integer(i) => Json::from(*i),
        toml::Value::Float(f) => Json::from(*f),
        toml::Value::Boolean(b) => Json::from(*b),
        toml::Value::Datetime(d) => Json::from(d.to_string()),
        toml::Value::Array(a) => Json::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            Json::Object(t.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect())
        }
    }
}

fn read_json_root(file: &Path) -> Result<Json> {
    if !file.exists() {
        return Ok(Json::Object(Default::default()));
    }
    let s = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    if s.trim().is_empty() {
        return Ok(Json::Object(Default::default()));
    }
    serde_json::from_str(&s).with_context(|| format!("parsing json {}", file.display()))
}

fn json_get<'a>(root: &'a Json, parts: &[&str]) -> Option<&'a Json> {
    let mut cur = root;
    for p in parts {
        cur = cur.as_object()?.get(*p)?;
    }
    Some(cur)
}

fn json_satisfied(root: &Json, parts: &[&str], value: &Json, append: bool) -> bool {
    match json_get(root, parts) {
        None => false,
        Some(cur) if append => cur.as_array().is_some_and(|a| a.contains(value)),
        Some(cur) => cur == value,
    }
}

fn json_set(root: &mut Json, parts: &[&str], value: Json, append: bool) -> Result<()> {
    if !root.is_object() {
        *root = Json::Object(Default::default());
    }
    let obj = root.as_object_mut().unwrap();
    if parts.len() == 1 {
        if append {
            let arr = obj.entry(parts[0].to_string()).or_insert(Json::Array(vec![]));
            let a = arr
                .as_array_mut()
                .ok_or_else(|| anyhow!("setkey append: `{}` is not an array", parts[0]))?;
            if !a.contains(&value) {
                a.push(value);
            }
        } else {
            obj.insert(parts[0].to_string(), value);
        }
        return Ok(());
    }
    let child = obj
        .entry(parts[0].to_string())
        .or_insert(Json::Object(Default::default()));
    json_set(child, &parts[1..], value, append)
}

pub fn setkey_state(sk: &SetKey) -> Result<FileState> {
    if sk.backend != "json" {
        bail!("setkey backend `{}` is not implemented yet (json only)", sk.backend);
    }
    let file = sk_file(sk)?;
    if !file.exists() {
        return Ok(FileState::Missing);
    }
    let root = read_json_root(&file)?;
    let parts: Vec<&str> = sk.key.split('.').collect();
    let value = toml_to_json(&sk.value);
    Ok(if json_satisfied(&root, &parts, &value, sk.append) {
        FileState::InSync
    } else {
        FileState::Drifted
    })
}

pub fn setkey_apply(sk: &SetKey, journal: &mut Journal) -> Result<bool> {
    if sk.backend != "json" {
        bail!("setkey backend `{}` is not implemented yet (json only)", sk.backend);
    }
    let file = sk_file(sk)?;
    let mut root = read_json_root(&file)?;
    let parts: Vec<&str> = sk.key.split('.').collect();
    let value = toml_to_json(&sk.value);
    if json_satisfied(&root, &parts, &value, sk.append) {
        return Ok(false);
    }
    json_set(&mut root, &parts, value, sk.append)?;
    let mut new = serde_json::to_string_pretty(&root)?;
    new.push('\n');
    let before = if file.exists() {
        Some(fs::read(&file)?)
    } else {
        None
    };
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    journal.record_write(&file, before.as_deref(), new.as_bytes())?;
    fs::write(&file, new.as_bytes())?;
    Ok(true)
}
