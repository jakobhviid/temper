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

/// A live spinner line on stderr — `⠋ Installing llvm` — for a long phase whose
/// individual items we learn about as they start (a `brew bundle` converge). The
/// tick chars, cadence, and cyan match `grove`'s fetch spinner so every tool in
/// the fleet animates identically.
///
/// stderr (never stdout), so `--json` and piped output stay clean; indicatif
/// hides the bar entirely when stderr isn't a terminal, which makes this inert in
/// CI and in the test suite.
pub fn spinner(msg: &str) -> indicatif::ProgressBar {
    let pb = indicatif::ProgressBar::new_spinner();
    pb.set_style(
        indicatif::ProgressStyle::with_template("  {spinner:.cyan} {msg}")
            .expect("static template")
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(90));
    pb
}

/// A spinner that also carries a `pos/len` counter — for a phase whose total is
/// known up front and whose items are installed one at a time (App Store apps via
/// `mas`), so a stalled download reads as "3/49", not as a hang.
pub fn spinner_counted(len: u64, msg: &str) -> indicatif::ProgressBar {
    let pb = indicatif::ProgressBar::new(len);
    pb.set_style(
        indicatif::ProgressStyle::with_template("  {spinner:.cyan} {msg} {pos}/{len}")
            .expect("static template")
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(90));
    pb
}
