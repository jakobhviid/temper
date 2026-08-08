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

// --- aligned columns ----------------------------------------------------------

/// Gap between columns.
const GAP: usize = 2;

/// The narrowest a flexing column may be squeezed before we stop trying.
const FLEX_MIN: usize = 12;

/// Display width — **not** byte length. `~/Bibliotek/Programstøtte/…` and the
/// `✓`/`⋯` glyphs are multi-byte, so `len()` would pad them short and leave the
/// columns visibly ragged in exactly the paths worth reading.
fn width(s: &str) -> usize {
    console::measure_text_width(s)
}

/// Terminal width, or `None` when stdout is not a terminal — which is also the
/// signal *not* to shorten anything: a redirected log or a pipe must keep the full
/// path, since eliding into a file destroys the evidence it was written to hold.
fn term_cols() -> Option<usize> {
    console::Term::stdout().size_checked().map(|(_, c)| c as usize)
}

/// Shorten to `max`, marking the cut with `…`. A path-ish cell loses its **head**
/// (`…/scripts/retire-sesh-tap.sh` still identifies the step, where
/// `assets/scripts/retire-…` does not); anything else loses its tail.
fn elide(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".into();
    }
    let keep = max - 1;
    let chars: Vec<char> = s.chars().collect();
    if s.contains('/') || s.starts_with('~') {
        let tail: String = chars[chars.len().saturating_sub(keep)..].iter().collect();
        format!("…{tail}")
    } else {
        let head: String = chars[..keep.min(chars.len())].iter().collect();
        format!("{head}…")
    }
}

/// Column widths for a table that is printed **one row at a time**.
///
/// Streaming output normally can't align — you'd have to buffer every row to learn
/// the widest cell. temper doesn't have to: it *plans before it applies*, so the
/// full item list exists before the first line prints (it is where the phase's
/// `[12/44]` denominator comes from). One pass over it gives exact widths, with no
/// buffering and no reflow.
///
/// Shared by the step phase and `drift` so the two finally read as one table:
/// same widths, same measurement, same elision rules. Their column *order* still
/// differs on purpose — `drift` groups under an app header, so repeating the app on
/// every row would be noise, while the step phase is a flat stream where it isn't.
pub struct Columns {
    widths: Vec<usize>,
    /// Printable width before column 0 (indent + marker), so a row can tell
    /// whether it still fits the terminal.
    prefix: usize,
    /// The column that gives way when the line would wrap.
    flex: usize,
}

impl Columns {
    /// Measure from every row the phase may print. `caps` clamps a column so one
    /// outlier (`desktop-overrides` next to `zsh`) can't shove the whole table
    /// right — an over-cap cell is elided to the cap and only its own row spills.
    /// `0` leaves a column uncapped.
    pub fn measure(rows: &[Vec<String>], prefix: usize, caps: &[usize], flex: usize) -> Columns {
        let n = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let mut widths = vec![0usize; n];
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(width(cell));
            }
        }
        for (i, w) in widths.iter_mut().enumerate() {
            if let Some(&cap) = caps.get(i) {
                if cap > 0 {
                    *w = (*w).min(cap);
                }
            }
        }
        // Nothing follows the last column, so padding it would only add trailing
        // whitespace — which shows up as diff noise the moment output is captured.
        if let Some(last) = widths.last_mut() {
            *last = 0;
        }
        Columns {
            widths,
            prefix,
            flex,
        }
    }

    /// One aligned row as `(cell, padding-after-it)` pairs.
    ///
    /// Split out from [`Columns::row`] for the callers that **colour** their cells:
    /// ANSI escapes have no display width, so the padding has to be computed from
    /// the plain text and the colour applied afterwards. Eliding has to happen on
    /// plain text too — slicing a coloured string would cut an escape sequence in
    /// half and bleed the colour down the rest of the terminal.
    pub fn parts(&self, cells: &[&str]) -> Vec<(String, usize)> {
        let cells = self.fitted(cells);
        let last = cells.len().saturating_sub(1);
        cells
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let pad = if i == last {
                    0
                } else {
                    self.widths[i].saturating_sub(width(cell)) + GAP
                };
                (cell.clone(), pad)
            })
            .collect()
    }

    /// One aligned row. Cells are padded to their column; the `flex` column gives
    /// way (elided) when the line would otherwise wrap the terminal.
    pub fn row(&self, cells: &[&str]) -> String {
        let mut out = String::new();
        for (cell, pad) in self.parts(cells) {
            out.push_str(&cell);
            out.push_str(&" ".repeat(pad));
        }
        out
    }

    /// Cells elided to their caps, and the flex column squeezed if the row would
    /// otherwise wrap. Plain text only — see [`Columns::parts`].
    fn fitted(&self, cells: &[&str]) -> Vec<String> {
        let mut cells: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| match self.widths.get(i) {
                // Over its cap → elide to the cap so the columns after it hold.
                Some(&w) if w > 0 => elide(c, w),
                _ => c.to_string(),
            })
            .collect();

        // Squeeze the flex column if the line would wrap. Only on a terminal:
        // redirected output keeps every character.
        if let (Some(cols), Some(flex)) = (term_cols(), cells.get(self.flex).cloned()) {
            let fixed: usize = self.prefix
                + cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let w = if i == self.flex {
                            0
                        } else {
                            width(c).max(self.widths.get(i).copied().unwrap_or(0))
                        };
                        w + GAP
                    })
                    .sum::<usize>();
            if fixed + width(&flex) > cols {
                let room = cols.saturating_sub(fixed).max(FLEX_MIN);
                cells[self.flex] = elide(&flex, room);
            }
        }
        cells
    }
}

/// Says what a phase is waiting on, for a unit that runs long enough that silence
/// would read as a hang — then gets out of the way.
///
/// This is the safe half of a spinner. A unit that may hand the terminal to
/// arbitrary code (an `exec` step) cannot carry an animated line: `sudo`/polkit/PAM
/// prompt on `/dev/tty` at a moment we cannot predict — in practice within the
/// first seconds — and anything of ours redrawing in place fuses onto that prompt.
/// So the line is written **once**.
///
/// It is deliberately **not** shaped like a row of the results list. The leftmost
/// glyph there is a *status* column — `✓`/`!`/`✗` — and the eye scans it for
/// exceptions, so a `⋯` sitting in it reads as "this step has a problem", which is
/// the opposite of what it means. Instead it is indented as a subordinate detail,
/// dimmed, and says in words what it is:
///
/// ```text
///   ✓ ptyxis             exec    assets/scripts/ptyxis-load.sh
///       … still working: assets/scripts/1password-setup.sh
/// Place your finger on the fingerprint reader
///   ✓ 1password          exec    assets/scripts/1password-setup.sh    12s
/// ```
///
/// Nothing is printed at all for a unit that finishes before the threshold, so the
/// quick ones stay a single `✓`. Dropping this cancels a notice that hasn't fired.
pub struct WaitNotice {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// A short, human duration: `12s`, `2m14s`. Whole seconds — this explains a pause,
/// it is not a benchmark.
fn human_secs(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

/// How long a unit may run before it says what it is.
const WAIT_NOTICE_AFTER: std::time::Duration = std::time::Duration::from_secs(3);

impl WaitNotice {
    pub fn new(label: &str) -> WaitNotice {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if json_mode() {
            return WaitNotice { done, handle: None };
        }
        let flag = done.clone();
        let line = dim(&format!("      … still working: {label}"));
        let handle = std::thread::spawn(move || {
            // Ticked rather than slept whole, so a fast unit's notice is cancelled
            // promptly instead of holding the thread for the full window.
            let tick = std::time::Duration::from_millis(100);
            let mut waited = std::time::Duration::ZERO;
            while waited < WAIT_NOTICE_AFTER {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(tick);
                waited += tick;
            }
            if !flag.load(std::sync::atomic::Ordering::Relaxed) {
                eprintln!("{line}"); // stderr: progress, so `--json` stays clean
            }
        });
        WaitNotice {
            done,
            handle: Some(handle),
        }
    }
}

impl Drop for WaitNotice {
    fn drop(&mut self) {
        self.done
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
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

    /// As [`Checklist::done`], with how long the unit took — shown only past the
    /// same threshold that prints a "still working" notice, so the pause a reader
    /// just sat through is accounted for on the line that resolves it, and quick
    /// units stay a bare `✓`.
    pub fn done_after(&self, label: &str, elapsed: std::time::Duration) {
        if elapsed < WAIT_NOTICE_AFTER {
            return self.done(label);
        }
        self.emit(format!(
            "  {} {label}  {}",
            green("✓"),
            dim(&human_secs(elapsed))
        ));
        self.advance();
    }

    /// The unit was already in sync. Counted, never printed: silence is how a
    /// converged machine reports itself.
    pub fn unchanged(&self) {
        self.advance();
    }

    /// A unit that was not applied, with the reason — loud by design
    /// (Principle #6). Callers phrase `why` fully ("binary `topgrade` absent",
    /// "changed since temper wrote it"), because only they know what it means.
    pub fn skipped(&self, label: &str, why: &str) {
        self.emit(format!("  {} {label} — skipped: {why}", yellow("⚠")));
        self.advance();
    }

    /// A unit a *dry run* would have acted on. Neither a change nor a warning, so
    /// it gets neither mark — the point is naming the items behind the count.
    pub fn noted(&self, label: &str) {
        self.emit(format!("  {} {label}", dim("·")));
        self.advance();
    }

    /// Run `f` with the live region **cleared**, for a unit that may talk to the
    /// terminal itself.
    ///
    /// An `exec` step runs arbitrary code, and what that code says to the human is
    /// not chatter to be hushed: `sudo`/polkit/PAM write prompts straight to
    /// `/dev/tty`, bypassing the pipes we capture, so they arrive *on top of* a
    /// live region — fusing "Place your finger on the fingerprint reader" onto the
    /// spinner's line, where the next 90 ms tick can erase the one message the run
    /// is waiting on, and leaving the half-drawn line behind as permanent debris.
    /// Clearing the region first gives the prompt a clean line of its own, keeps it
    /// on screen, and leaves nothing fused behind it.
    pub fn suspend<R>(&self, f: impl FnOnce() -> R) -> R {
        match &self.pb {
            Some(pb) => pb.suspend(f),
            None => f(),
        }
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

#[cfg(test)]
mod column_tests {
    use super::*;

    /// The display column a substring starts at (as against its byte offset).
    fn col_of(line: &str, needle: &str) -> Option<usize> {
        line.find(needle).map(|b| width(&line[..b]))
    }

    fn rows(pairs: &[(&str, &str)]) -> Vec<Vec<String>> {
        pairs
            .iter()
            .map(|(a, b)| vec![a.to_string(), b.to_string(), String::new()])
            .collect()
    }

    #[test]
    fn columns_align_to_the_widest_cell() {
        let r = rows(&[("zsh", "copy"), ("desktop-overrides", "exec"), ("ssh", "block")]);
        let c = Columns::measure(&r, 4, &[16, 0, 0], 2);
        let a = c.row(&["zsh", "copy", "~/.zshrc"]);
        let b = c.row(&["ssh", "block", "~/.ssh/config"]);
        // Every row puts its kind — and therefore its target — at the same column.
        // Compared in display columns: `find` yields *bytes*, the very measure
        // `width()` exists to avoid.
        assert_eq!(
            col_of(&a, "copy"),
            col_of(&b, "block"),
            "kind column not aligned:\n{a}\n{b}"
        );
    }

    #[test]
    fn a_long_name_is_capped_not_allowed_to_shove_everything_right() {
        let r = rows(&[("zsh", "copy"), ("an-absurdly-long-bundle-name", "exec")]);
        let c = Columns::measure(&r, 4, &[16, 0, 0], 2);
        let short = c.row(&["zsh", "copy", "~/.zshrc"]);
        // The cap, not the outlier, decides the column: "zsh" padded to 16 + gap.
        assert_eq!(col_of(&short, "copy"), Some(18), "{short:?}");
        // The outlier is elided to the cap rather than widening the table.
        let long = c.row(&["an-absurdly-long-bundle-name", "exec", "x"]);
        assert!(long.starts_with("an-absurdly-lon…"), "{long:?}");
        assert_eq!(col_of(&long, "exec"), Some(18), "{long:?}");
    }

    #[test]
    fn width_is_measured_in_display_columns_not_bytes() {
        // Danish paths are the everyday case: `ø` is two bytes, one column. Padding
        // by byte length would silently shorten the pad and skew the table.
        let r = rows(&[("søg", "copy"), ("zsh", "copy")]);
        let c = Columns::measure(&r, 4, &[16, 0, 0], 2);
        let a = c.row(&["søg", "copy", "~/x"]);
        let b = c.row(&["zsh", "copy", "~/x"]);
        assert_eq!(col_of(&a, "copy"), col_of(&b, "copy"), "\n{a}\n{b}");
        // …and the byte offsets differ, which is why this is worth a test at all.
        assert_ne!(a.find("copy"), b.find("copy"));
    }

    #[test]
    fn a_path_loses_its_head_a_name_loses_its_tail() {
        // The tail identifies a path; the head identifies a name.
        assert_eq!(elide("assets/scripts/retire-sesh-tap.sh", 12), "…sesh-tap.sh");
        assert_eq!(elide("desktop-overrides", 12), "desktop-ove…");
        assert_eq!(width(&elide("assets/scripts/x.sh", 12)), 12);
        assert_eq!(elide("~/.config/x", 40), "~/.config/x"); // fits → untouched
    }

    #[test]
    fn redirected_output_is_never_shortened() {
        // The test harness's stdout is not a terminal, which is exactly the case
        // that must keep every character: a log is evidence.
        assert!(term_cols().is_none(), "test stdout should not be a tty");
        let r = rows(&[("zsh", "copy")]);
        let c = Columns::measure(&r, 4, &[16, 0, 0], 2);
        let long = "~/".to_string() + &"x/".repeat(200);
        assert!(c.row(&["zsh", "copy", &long]).contains(&long));
    }
}
