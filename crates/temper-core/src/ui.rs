//! Output discipline so `--json` stays pipe-clean: human output → stdout,
//! progress + errors → stderr (amdl's rule). Plus a tiny ANSI palette for the
//! human (non-json) renderers.
//!
//! Colour is emitted only when stdout is a real terminal AND `NO_COLOR` is
//! unset — so a redirect / pipe (including the `--json` path, which never calls
//! these) stays clean. The decision is computed once.

use std::io::IsTerminal;
use std::sync::OnceLock;

/// Whether this process is emitting `--json`. Set once at startup, before any
/// output. Everything that prints *during* a run (progress regions, per-item
/// lines) consults this, so `--json` stdout stays exactly one document without
/// every call site having to carry the flag down to where it prints.
static JSON: OnceLock<bool> = OnceLock::new();

pub fn set_json(on: bool) {
    let _ = JSON.set(on);
}

/// Whether stdout is reserved for a single `--json` document.
pub fn json_mode() -> bool {
    *JSON.get().unwrap_or(&false)
}

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

/// Live progress for a phase of discrete units — the config-step phase, a
/// per-file restore, a per-entry undo — where the total is known up front.
///
/// Two surfaces, deliberately different in lifetime:
///
/// - a **transient** one-line region on stderr (`⠹ [12/26] zsh · copy ~/.zshrc
///   0:02:14`), erased when the phase ends. It answers "what is it doing right
///   now, and is it still alive" — the question a captured multi-minute child
///   otherwise leaves unanswered. `{wide_msg}` truncates to the terminal width,
///   so a long path can't wrap and leave debris behind the redraw.
/// - a **permanent** `✓` line on stdout for each unit that actually changed
///   something. A converged machine emits none of them and stays silent, which is
///   the same contract the summary count has always described — this only makes it
///   visible per item instead of totalled at the end.
///
/// Inert under `--json` (stdout carries one document) and spinner-free under
/// `--verbose` (children stream their own output there, and a live region would
/// fight them for the cursor — the `✓` lines still print).
pub struct Checklist {
    pb: Option<indicatif::ProgressBar>,
}

impl Checklist {
    /// `len` units in `phase`. A zero-unit phase gets no region at all.
    pub fn new(len: usize, phase: &str, verbose: bool) -> Checklist {
        let live = len > 0 && !verbose && !json_mode();
        let pb = live.then(|| {
            let pb = indicatif::ProgressBar::new(len as u64);
            pb.set_style(
                indicatif::ProgressStyle::with_template(
                    "  {spinner:.cyan} [{pos}/{len}] {wide_msg} {elapsed}",
                )
                .expect("static template")
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
            );
            pb.set_message(phase.to_string());
            pb.enable_steady_tick(std::time::Duration::from_millis(90));
            pb
        });
        Checklist { pb }
    }

    /// Print through the live region rather than over it.
    fn emit(&self, line: String) {
        if json_mode() {
            return;
        }
        match &self.pb {
            Some(pb) => pb.suspend(|| println!("{line}")),
            None => println!("{line}"),
        }
    }

    /// Name the unit about to be worked on.
    pub fn start(&self, label: &str) {
        if let Some(pb) = &self.pb {
            pb.set_message(label.to_string());
        }
    }

    /// The unit changed something — it earns a permanent line.
    pub fn done(&self, label: &str) {
        self.emit(format!("  {} {label}", green("✓")));
        self.advance();
    }

    /// The unit was already in sync. Counted, never printed: silence is how a
    /// converged machine reports itself.
    pub fn unchanged(&self) {
        self.advance();
    }

    /// A unit skipped by a failed presence gate — loud by design (Principle #6).
    /// `why` is a probe description (`binary \`topgrade\``), so it reads as
    /// "…skipped: binary `topgrade` absent".
    pub fn skipped(&self, label: &str, why: &str) {
        self.emit(format!("  {} {label} — skipped: {why} absent", yellow("⚠")));
        self.advance();
    }

    /// A warning from inside the phase, kept off the region's line.
    pub fn warn(&self, msg: &str) {
        match &self.pb {
            Some(pb) => pb.suspend(|| eprintln!("  {} {msg}", yellow("⚠"))),
            None => eprintln!("  {} {msg}", yellow("⚠")),
        }
    }

    fn advance(&self) {
        if let Some(pb) = &self.pb {
            pb.inc(1);
        }
    }

    /// Erase the region — the permanent lines and the summary stay.
    pub fn finish(self) {
        if let Some(pb) = self.pb {
            pb.finish_and_clear();
        }
    }
}
