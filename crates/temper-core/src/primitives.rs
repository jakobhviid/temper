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
    /// Can't be evaluated here (the backend's tool is absent, e.g. dconf on a
    /// Mac host). Not a failure — degrade, don't abort.
    Unavailable,
}

impl FileState {
    pub fn label(self) -> &'static str {
        match self {
            FileState::Missing => "missing",
            FileState::InSync => "in sync",
            FileState::Drifted => "drifted",
            FileState::Unavailable => "unavailable",
        }
    }

    pub fn is_ok(self) -> bool {
        matches!(self, FileState::InSync | FileState::Unavailable)
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

/// `copy` apply. Returns whether the file's content changed. A `seed` target
/// that already exists is left entirely untouched (hands-off — including its
/// mode); for a non-seed copy, `mode` is enforced idempotently.
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
    // Argless functions resolve live state: `{{ brew_prefix }}` is the ergonomic
    // fix for the Mac/Linux Homebrew-prefix split (a byte-different value on each
    // OS) — no per-machine `var` needed.
    if !expr.contains(char::is_whitespace) {
        return match expr {
            "brew_prefix" => brew_prefix(),
            other => bail!("unknown template function `{other}`"),
        };
    }
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

/// `brew --prefix` (e.g. `/opt/homebrew` on Apple Silicon,
/// `/home/linuxbrew/.linuxbrew` on Linux). Errors loudly if brew is absent, so a
/// template that needs it fails visibly rather than rendering an empty path.
fn brew_prefix() -> Result<String> {
    let out = std::process::Command::new("brew")
        .arg("--prefix")
        .output()
        .map_err(|_| anyhow!("brew_prefix: `brew` not found on PATH"))?;
    if !out.status.success() {
        bail!("brew_prefix: `brew --prefix` failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
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

#[cfg(test)]
mod render_tests {
    use super::*;

    #[test]
    fn brew_prefix_renders_when_brew_present() {
        // Live check: only meaningful where brew exists (this dev/Bazzite box).
        if which("brew").is_none() {
            return;
        }
        let out = render("prefix={{ brew_prefix }}", &BTreeMap::new()).unwrap();
        assert!(out.starts_with("prefix=/"), "got {out:?}");
        assert!(!out.contains("{{"), "unrendered template: {out:?}");
    }

    #[test]
    fn unknown_argless_function_errors() {
        assert!(render("{{ bogus_fn }}", &BTreeMap::new()).is_err());
    }

    #[test]
    fn var_still_works_alongside_argless() {
        let mut vars = BTreeMap::new();
        vars.insert("NAME".to_string(), "kira".to_string());
        assert_eq!(render("hi {{ var \"NAME\" }}", &vars).unwrap(), "hi kira");
    }
}

// --- block: ensure a marker-delimited region is present in a user file --------

fn markers(marker: &str) -> (String, String) {
    (
        format!("# >>> temper:{marker} >>>"),
        format!("# <<< temper:{marker} <<<"),
    )
}

/// The file content that should result from ensuring `body` sits inside the
/// marker region — replacing a well-formed existing region or appending a new
/// one. Refuses (errors) on a malformed region rather than silently deleting
/// user content or growing the file on every run.
fn block_desired(existing: &str, begin: &str, end: &str, body: &str) -> Result<String> {
    if body.contains(begin) || body.contains(end) {
        bail!("block body contains the marker delimiter — pick a different marker name");
    }
    let region = format!("{begin}\n{}\n{end}", body.trim_end_matches('\n'));
    match (existing.find(begin), existing.find(end)) {
        // A well-formed region: replace it in place.
        (Some(bs), Some(es)) if es > bs => {
            let region_end = es + end.len();
            let mut out = String::with_capacity(existing.len() + region.len());
            out.push_str(&existing[..bs]);
            out.push_str(&region);
            out.push_str(&existing[region_end..]);
            Ok(out)
        }
        // No region yet: append one.
        (None, None) => {
            let mut out = existing.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&region);
            out.push('\n');
            Ok(out)
        }
        // Orphaned/out-of-order markers: refuse rather than corrupt.
        _ => bail!(
            "malformed marker region (unbalanced or out-of-order `{begin}` / `{end}`) — \
             fix or remove it by hand"
        ),
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
    let want = block_desired(&existing, &begin, &end, &body)
        .with_context(|| format!("in {}", target.display()))?;
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
    let want = block_desired(&existing, &begin, &end, &body)
        .with_context(|| format!("in {}", target.display()))?;
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
    // root is guaranteed an object by the caller (json_apply guards it, and each
    // recursion guards the child below), so this never clobbers real data.
    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("setkey json: `{}` is not an object", parts[0]))?;
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
    // Refuse to descend into (and clobber) an existing scalar intermediate.
    if !child.is_object() {
        bail!("setkey json: intermediate key `{}` is not an object", parts[0]);
    }
    json_set(child, &parts[1..], value, append)
}

pub fn setkey_state(sk: &SetKey) -> Result<FileState> {
    match sk.backend.as_str() {
        "json" => json_state(sk),
        "toml" => toml_state(sk),
        "ini" | "desktop" => ini_state(sk),
        "defaults" => defaults_state(sk),
        "dconf" => dconf_state(sk),
        other => bail!("setkey backend `{other}` is not recognized"),
    }
}

fn json_state(sk: &SetKey) -> Result<FileState> {
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

/// Read a file's prior bytes, journal the write, and write the new bytes.
fn journaled_write(file: &Path, new: &[u8], journal: &mut Journal) -> Result<bool> {
    let before = if file.exists() {
        Some(fs::read(file)?)
    } else {
        None
    };
    if before.as_deref() == Some(new) {
        return Ok(false);
    }
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    journal.record_write(file, before.as_deref(), new)?;
    fs::write(file, new)?;
    Ok(true)
}

// --- exec: run a user script (the escape hatch) -------------------------------

/// Execution context for `exec`/`check` scripts.
pub struct ExecOpts<'a> {
    pub secrets: &'a [String],
    pub home: &'a Path,
    pub machine: &'a str,
    pub os: &'a str,
}

fn exec_command(script: &Path, opts: &ExecOpts) -> Result<std::process::Command> {
    use std::process::Command;
    // Always run as the invoking user — the chezmoi/Ansible model. A script that
    // needs root for specific commands escalates INTERNALLY with `sudo <cmd>`
    // (never the whole script, which would break user-session ops like
    // gsettings / D-Bus / ~/ file writes by running them as root).
    let mut cmd = Command::new("sh");
    cmd.arg(script);
    cmd.current_dir(opts.home);
    cmd.env("TEMPER_HOME", opts.home);
    cmd.env("TEMPER_MACHINE", opts.machine);
    cmd.env("TEMPER_OS", opts.os);
    for s in opts.secrets {
        let v = std::env::var(s)
            .map_err(|_| anyhow!("exec: required secret env `{s}` is not set"))?;
        cmd.env(s, v);
    }
    Ok(cmd)
}

/// Run a drift-hook: true if it exits 0 (in sync).
pub fn exec_check(check: &Path, opts: &ExecOpts) -> Result<bool> {
    let status = exec_command(check, opts)?
        .status()
        .with_context(|| format!("running check {}", check.display()))?;
    Ok(status.success())
}

/// Apply an `exec` step. If a `check` is given and already passes, the script is
/// skipped (in sync). Otherwise the script runs; a non-zero exit is an error.
/// Returns whether the script ran. Not journaled — exec is imperative.
pub fn exec_apply(script: &Path, check: Option<&Path>, opts: &ExecOpts) -> Result<bool> {
    if let Some(check) = check {
        if exec_check(check, opts)? {
            return Ok(false);
        }
    }
    let status = exec_command(script, opts)?
        .status()
        .with_context(|| format!("running {}", script.display()))?;
    if !status.success() {
        bail!("exec {} failed ({})", script.display(), status);
    }
    Ok(true)
}

/// Drift state for an `exec` step: uses the `check` hook if present, else
/// reports that there is no drift story (visible, not failing).
pub fn exec_state(check: Option<&Path>, opts: &ExecOpts) -> Result<(bool, String)> {
    match check {
        Some(check) => Ok(if exec_check(check, opts)? {
            (true, "in sync".into())
        } else {
            (false, "drifted".into())
        }),
        None => Ok((true, "no drift-check".into())),
    }
}

pub fn setkey_apply(sk: &SetKey, journal: &mut Journal) -> Result<bool> {
    match sk.backend.as_str() {
        "json" => json_apply(sk, journal),
        "toml" => toml_apply(sk, journal),
        "ini" | "desktop" => ini_apply(sk, journal),
        "defaults" => defaults_apply(sk),
        "dconf" => dconf_apply(sk),
        other => bail!("setkey backend `{other}` is not recognized"),
    }
}

fn json_apply(sk: &SetKey, journal: &mut Journal) -> Result<bool> {
    let file = sk_file(sk)?;
    let mut root = read_json_root(&file)?;
    // read_json_root gives {} for absent/empty; a non-object here means the file
    // has real array/scalar content — refuse rather than overwrite it wholesale.
    if !root.is_object() {
        bail!("setkey json: {} root is not an object", file.display());
    }
    let parts: Vec<&str> = sk.key.split('.').collect();
    let value = toml_to_json(&sk.value);
    if json_satisfied(&root, &parts, &value, sk.append) {
        return Ok(false);
    }
    json_set(&mut root, &parts, value, sk.append)?;
    let mut new = serde_json::to_string_pretty(&root)?;
    new.push('\n');
    journaled_write(&file, new.as_bytes(), journal)
}

// --- setkey: toml backend -----------------------------------------------------

fn read_toml_root(file: &Path) -> Result<toml::Value> {
    if !file.exists() {
        return Ok(toml::Value::Table(Default::default()));
    }
    let s = fs::read_to_string(file)?;
    if s.trim().is_empty() {
        return Ok(toml::Value::Table(Default::default()));
    }
    toml::from_str(&s).with_context(|| format!("parsing toml {}", file.display()))
}

fn toml_get<'a>(root: &'a toml::Value, parts: &[&str]) -> Option<&'a toml::Value> {
    let mut cur = root;
    for p in parts {
        cur = cur.as_table()?.get(*p)?;
    }
    Some(cur)
}

fn toml_satisfied(root: &toml::Value, parts: &[&str], value: &toml::Value, append: bool) -> bool {
    match toml_get(root, parts) {
        None => false,
        Some(cur) if append => cur.as_array().is_some_and(|a| a.contains(value)),
        Some(cur) => cur == value,
    }
}

/// Convert a `toml::Value` into a `toml_edit::Value` (for comment-preserving
/// writes). Tables become inline tables (a leaf setkey value is a scalar/array
/// in practice; nested tables are created as intermediates by the navigator).
fn toml_to_edit_value(v: &toml::Value) -> toml_edit::Value {
    use toml_edit::Value as EV;
    match v {
        toml::Value::String(s) => EV::from(s.as_str()),
        toml::Value::Integer(i) => EV::from(*i),
        toml::Value::Float(f) => EV::from(*f),
        toml::Value::Boolean(b) => EV::from(*b),
        toml::Value::Datetime(d) => EV::from(d.to_string()),
        toml::Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for x in a {
                arr.push(toml_to_edit_value(x));
            }
            EV::Array(arr)
        }
        toml::Value::Table(t) => {
            let mut it = toml_edit::InlineTable::new();
            for (k, val) in t {
                it.insert(k, toml_to_edit_value(val));
            }
            EV::InlineTable(it)
        }
    }
}

/// Set a dotted key in a `toml_edit` document, creating intermediate tables and
/// preserving all surrounding comments/formatting. `append` array-unions.
fn toml_edit_set(
    doc: &mut toml_edit::DocumentMut,
    parts: &[&str],
    value: &toml::Value,
    append: bool,
) -> Result<()> {
    let mut tbl = doc.as_table_mut();
    for p in &parts[..parts.len() - 1] {
        let entry = tbl
            .entry(p)
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        tbl = entry
            .as_table_mut()
            .ok_or_else(|| anyhow!("setkey toml: intermediate key `{p}` is not a table"))?;
    }
    let last = parts[parts.len() - 1];
    if append {
        let item = tbl.entry(last).or_insert(toml_edit::Item::Value(
            toml_edit::Value::Array(toml_edit::Array::new()),
        ));
        let arr = item
            .as_array_mut()
            .ok_or_else(|| anyhow!("setkey append: `{last}` is not an array"))?;
        let ev = toml_to_edit_value(value);
        let evs = ev.to_string();
        if !arr.iter().any(|x| x.to_string().trim() == evs.trim()) {
            arr.push(ev);
        }
    } else {
        tbl.insert(last, toml_edit::Item::Value(toml_to_edit_value(value)));
    }
    Ok(())
}

#[cfg(test)]
mod toml_edit_tests {
    use super::*;

    #[test]
    fn preserves_comments_on_set() {
        let src = "# top comment\n[tool]\nname = \"a\" # inline note\nother = 1\n";
        let mut doc: toml_edit::DocumentMut = src.parse().unwrap();
        toml_edit_set(&mut doc, &["tool", "name"], &toml::Value::String("b".into()), false).unwrap();
        let out = doc.to_string();
        assert!(out.contains("# top comment"), "lost top comment: {out}");
        assert!(out.contains("other = 1"), "lost sibling: {out}");
        assert!(out.contains("\"b\""), "value not set: {out}");
    }

    #[test]
    fn append_dedups() {
        let mut doc: toml_edit::DocumentMut = "list = [\"a\"]\n".parse().unwrap();
        toml_edit_set(&mut doc, &["list"], &toml::Value::String("b".into()), true).unwrap();
        toml_edit_set(&mut doc, &["list"], &toml::Value::String("b".into()), true).unwrap();
        assert_eq!(doc.to_string().matches("\"b\"").count(), 1, "append must dedup");
    }

    #[test]
    fn creates_nested_tables() {
        let mut doc: toml_edit::DocumentMut = "".parse().unwrap();
        toml_edit_set(&mut doc, &["a", "b", "c"], &toml::Value::Integer(5), false).unwrap();
        assert_eq!(doc["a"]["b"]["c"].as_integer(), Some(5));
    }
}

fn toml_state(sk: &SetKey) -> Result<FileState> {
    let file = sk_file(sk)?;
    if !file.exists() {
        return Ok(FileState::Missing);
    }
    let root = read_toml_root(&file)?;
    let parts: Vec<&str> = sk.key.split('.').collect();
    Ok(if toml_satisfied(&root, &parts, &sk.value, sk.append) {
        FileState::InSync
    } else {
        FileState::Drifted
    })
}

fn toml_apply(sk: &SetKey, journal: &mut Journal) -> Result<bool> {
    let file = sk_file(sk)?;
    let root = read_toml_root(&file)?;
    if !root.is_table() {
        bail!("setkey toml: {} root is not a table", file.display());
    }
    let parts: Vec<&str> = sk.key.split('.').collect();
    if toml_satisfied(&root, &parts, &sk.value, sk.append) {
        return Ok(false);
    }
    // Edit through toml_edit so hand comments + formatting survive the write
    // (the plain `toml` reserialize used to drop them).
    let existing = if file.exists() {
        fs::read_to_string(&file)?
    } else {
        String::new()
    };
    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .with_context(|| format!("parsing toml {}", file.display()))?;
    toml_edit_set(&mut doc, &parts, &sk.value, sk.append)?;
    journaled_write(&file, doc.to_string().as_bytes(), journal)
}

// --- setkey: ini / .desktop backend -------------------------------------------
// Key is "Section.Key" (e.g. "Desktop Entry.Icon") or a bare "Key" (no section).

fn ini_split(key: &str) -> (Option<&str>, &str) {
    match key.rsplit_once('.') {
        Some((section, k)) => (Some(section), k),
        None => (None, key),
    }
}

/// A scalar value rendered for an INI line.
fn scalar_str(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn ini_current<'a>(content: &'a str, section: Option<&str>, key: &str) -> Option<&'a str> {
    let mut in_section = section.is_none();
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            in_section = section == Some(&t[1..t.len() - 1]);
            continue;
        }
        if in_section {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == key {
                    return Some(v.trim());
                }
            }
        }
    }
    None
}

fn ini_desired(content: &str, section: Option<&str>, key: &str, value: &str) -> String {
    let want = format!("{key}={value}");
    let mut out: Vec<String> = Vec::new();
    let mut in_section = section.is_none();
    let mut done = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if in_section && !done {
                out.push(want.clone());
                done = true;
            }
            in_section = section == Some(&t[1..t.len() - 1]);
            out.push(line.to_string());
            continue;
        }
        if in_section && !done {
            if let Some((k, _)) = line.split_once('=') {
                if k.trim() == key {
                    out.push(want.clone());
                    done = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    if !done {
        if in_section {
            out.push(want);
        } else if let Some(s) = section {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(format!("[{s}]"));
            out.push(want);
        } else {
            out.push(want);
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

fn ini_state(sk: &SetKey) -> Result<FileState> {
    let file = sk_file(sk)?;
    if !file.exists() {
        return Ok(FileState::Missing);
    }
    let content = fs::read_to_string(&file)?;
    let (section, key) = ini_split(&sk.key);
    Ok(if ini_current(&content, section, key) == Some(scalar_str(&sk.value).as_str()) {
        FileState::InSync
    } else {
        FileState::Drifted
    })
}

fn ini_apply(sk: &SetKey, journal: &mut Journal) -> Result<bool> {
    let file = sk_file(sk)?;
    let content = if file.exists() {
        fs::read_to_string(&file)?
    } else {
        String::new()
    };
    let (section, key) = ini_split(&sk.key);
    let value = scalar_str(&sk.value);
    let new = ini_desired(&content, section, key, &value);
    journaled_write(&file, new.as_bytes(), journal)
}

// --- setkey: macOS `defaults` backend -----------------------------------------
// `file` is a domain (com.foo.bar) or a plist path. Not journaled (system-side).

fn defaults_read(target: &str, key: &str) -> Option<String> {
    let out = std::process::Command::new("defaults")
        .args(["read", target, key])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn defaults_target(sk: &SetKey) -> Result<String> {
    let raw = sk
        .file
        .clone()
        .ok_or_else(|| anyhow!("setkey(defaults) needs `file` (a domain or plist path)"))?;
    // A path target (~/… or /…) is expanded; a bare domain (com.foo.bar) is left as-is.
    Ok(if raw.starts_with("~/") || raw.starts_with('/') {
        crate::manifest::expand_tilde(&raw).to_string_lossy().into_owned()
    } else {
        raw
    })
}

/// Compare `defaults read` output to the wanted value. `defaults` stores bools
/// as "1"/"0", so a Boolean(true) must match "1", not "true".
fn defaults_matches(have: &str, want: &toml::Value) -> bool {
    match want {
        toml::Value::Boolean(b) => have == if *b { "1" } else { "0" },
        other => have == scalar_str(other),
    }
}

fn defaults_state(sk: &SetKey) -> Result<FileState> {
    if which("defaults").is_none() {
        return Ok(FileState::Unavailable); // degrade, don't abort (e.g. on Linux)
    }
    let target = defaults_target(sk)?;
    Ok(match defaults_read(&target, &sk.key) {
        None => FileState::Missing,
        Some(have) if defaults_matches(&have, &sk.value) => FileState::InSync,
        Some(_) => FileState::Drifted,
    })
}

fn defaults_apply(sk: &SetKey) -> Result<bool> {
    if matches!(defaults_state(sk)?, FileState::InSync | FileState::Unavailable) {
        return Ok(false);
    }
    let target = defaults_target(sk)?;
    let (flag, val) = match &sk.value {
        toml::Value::Boolean(b) => ("-bool", b.to_string()),
        toml::Value::Integer(i) => ("-int", i.to_string()),
        toml::Value::Float(f) => ("-float", f.to_string()),
        other => ("-string", scalar_str(other)),
    };
    let status = std::process::Command::new("defaults")
        .args(["write", &target, &sk.key, flag, &val])
        .status()
        .context("running defaults write")?;
    if !status.success() {
        bail!("defaults write {target} {} failed", sk.key);
    }
    Ok(true)
}

// --- setkey: dconf backend (Linux) --------------------------------------------
// Not journaled (system-side). VM-verified.

fn dconf_read(key: &str) -> Option<String> {
    let out = std::process::Command::new("dconf")
        .args(["read", key])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// Escape a string for a single-quoted GVariant literal.
fn gvariant_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Render a toml scalar as a GVariant literal for `dconf write`.
fn gvariant(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => format!("'{}'", gvariant_escape(s)),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Array(a) => {
            let items: Vec<String> = a.iter().map(gvariant).collect();
            format!("[{}]", items.join(", "))
        }
        other => format!("'{}'", gvariant_escape(&other.to_string())),
    }
}

fn dconf_state(sk: &SetKey) -> Result<FileState> {
    if which("dconf").is_none() {
        return Ok(FileState::Unavailable); // degrade, don't abort (e.g. on Mac)
    }
    let want = gvariant(&sk.value);
    Ok(match dconf_read(&sk.key) {
        None => FileState::Missing,
        Some(have) if have == want => FileState::InSync,
        Some(_) => FileState::Drifted,
    })
}

fn dconf_apply(sk: &SetKey) -> Result<bool> {
    if matches!(dconf_state(sk)?, FileState::InSync | FileState::Unavailable) {
        return Ok(false);
    }
    let status = std::process::Command::new("dconf")
        .args(["write", &sk.key, &gvariant(&sk.value)])
        .status()
        .context("running dconf write")?;
    if !status.success() {
        bail!("dconf write {} failed", sk.key);
    }
    Ok(true)
}

// --- sysfile: write one root-owned system file (the clean /etc path) ----------
// Not journaled (system-side, like defaults/dconf). Applies via `sudo install`,
// escalating for just that one write; drift compares content + mode + owner.

/// Options for a `sysfile` step.
pub struct SysfileOpts<'a> {
    pub mode: Option<&'a str>,
    pub owner: Option<&'a str>,
    pub group: Option<&'a str>,
}

/// The `sudo install` argv for an escalated system-file write. `-D` creates
/// parent dirs; mode/owner/group are applied atomically by `install` itself.
fn sysfile_install_argv(src: &Path, dest: &Path, opts: &SysfileOpts) -> Vec<String> {
    let mut a: Vec<String> = ["sudo", "install", "-D"].iter().map(|s| s.to_string()).collect();
    if let Some(m) = opts.mode {
        a.push("-m".into());
        a.push(m.to_string());
    }
    if let Some(o) = opts.owner {
        a.push("-o".into());
        a.push(o.to_string());
    }
    if let Some(g) = opts.group {
        a.push("-g".into());
        a.push(g.to_string());
    }
    a.push(src.to_string_lossy().into_owned());
    a.push(dest.to_string_lossy().into_owned());
    a
}

fn uid_of(name: &str) -> Option<u32> {
    std::process::Command::new("id")
        .args(["-u", name])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
}

fn gid_of(name: &str) -> Option<u32> {
    // `getent group <name>` → "name:x:GID:members"
    std::process::Command::new("getent")
        .args(["group", name])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split(':')
                .nth(2)
                .and_then(|g| g.trim().parse().ok())
        })
}

/// Drift for a `sysfile`: Missing (absent), Unavailable (present but unreadable
/// without escalation — degrade, don't prompt), else content + mode + owner/group.
pub fn sysfile_state(src: &Path, dest: &Path, opts: &SysfileOpts) -> Result<FileState> {
    if !dest.exists() {
        return Ok(FileState::Missing);
    }
    let want = fs::read(src).with_context(|| format!("reading source {}", src.display()))?;
    let have = match fs::read(dest) {
        Ok(h) => h,
        Err(_) => return Ok(FileState::Unavailable), // unreadable → don't sudo on a drift
    };
    if have != want {
        return Ok(FileState::Drifted);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        let md = fs::metadata(dest)?;
        if let Some(m) = opts.mode {
            let want = u32::from_str_radix(m.strip_prefix("0o").unwrap_or(m), 8)?;
            if md.permissions().mode() & 0o777 != want {
                return Ok(FileState::Drifted);
            }
        }
        if let Some(o) = opts.owner {
            if uid_of(o).is_some_and(|u| u != md.uid()) {
                return Ok(FileState::Drifted);
            }
        }
        if let Some(g) = opts.group {
            if gid_of(g).is_some_and(|gid| gid != md.gid()) {
                return Ok(FileState::Drifted);
            }
        }
    }
    Ok(FileState::InSync)
}

/// Apply a `sysfile`: idempotent — a no-op when already in sync, else an
/// escalated `sudo install`. Returns whether it changed anything. Not journaled.
pub fn sysfile_apply(src: &Path, dest: &Path, opts: &SysfileOpts) -> Result<bool> {
    if matches!(sysfile_state(src, dest, opts)?, FileState::InSync) {
        return Ok(false);
    }
    let argv = sysfile_install_argv(src, dest, opts);
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| format!("running {}", argv.join(" ")))?;
    if !status.success() {
        bail!("sudo install of {} failed", dest.display());
    }
    Ok(true)
}

#[cfg(test)]
mod sysfile_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn install_argv_shape() {
        let opts = SysfileOpts { mode: Some("0755"), owner: Some("root"), group: Some("root") };
        let argv = sysfile_install_argv(&PathBuf::from("/s"), &PathBuf::from("/etc/x"), &opts);
        assert_eq!(
            argv,
            vec!["sudo", "install", "-D", "-m", "0755", "-o", "root", "-g", "root", "/s", "/etc/x"]
        );
        // minimal (no mode/owner/group)
        let bare = SysfileOpts { mode: None, owner: None, group: None };
        let argv = sysfile_install_argv(&PathBuf::from("/s"), &PathBuf::from("/d"), &bare);
        assert_eq!(argv, vec!["sudo", "install", "-D", "/s", "/d"]);
    }

    #[test]
    fn state_missing_then_in_sync_without_escalation() {
        // A user-owned dest (no owner/group) exercises drift with no sudo.
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        fs::write(&src, b"policy\n").unwrap();
        let opts = SysfileOpts { mode: None, owner: None, group: None };
        assert_eq!(sysfile_state(&src, &dest, &opts).unwrap(), FileState::Missing);
        fs::write(&dest, b"policy\n").unwrap();
        assert_eq!(sysfile_state(&src, &dest, &opts).unwrap(), FileState::InSync);
        fs::write(&dest, b"tampered\n").unwrap();
        assert_eq!(sysfile_state(&src, &dest, &opts).unwrap(), FileState::Drifted);
    }
}

// --- profile: install a macOS .mobileconfig -----------------------------------
// Apply opens it in System Settings for the user to approve — installation
// can't be silently scripted without MDM, so drift is status-only ("manual").

pub fn profile_apply(file: &Path) -> Result<bool> {
    if which("open").is_none() {
        bail!("profile install needs macOS `open`");
    }
    let status = std::process::Command::new("open")
        .arg(file)
        .status()
        .with_context(|| format!("opening {}", file.display()))?;
    if !status.success() {
        bail!("open {} failed", file.display());
    }
    Ok(true)
}
