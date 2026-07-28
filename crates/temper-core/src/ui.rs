//! Output discipline so `--json` stays pipe-clean: human output → stdout,
//! progress + errors → stderr (amdl's rule). Plus a tiny ANSI palette for the
//! human (non-json) renderers.
//!
//! Colour is emitted only when stdout is a real terminal AND `NO_COLOR` is
//! unset — so a redirect / pipe (including the `--json` path, which never calls
//! these) stays clean. The decision is computed once.

use std::io::IsTerminal;
use std::sync::OnceLock;

fn color_enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}

fn paint(code: &str, s: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn green(s: &str) -> String {
    paint("1;32", s)
}
pub fn red(s: &str) -> String {
    paint("1;31", s)
}
pub fn yellow(s: &str) -> String {
    paint("1;33", s)
}
pub fn cyan(s: &str) -> String {
    paint("1;36", s)
}
pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn dim(s: &str) -> String {
    paint("2", s)
}
