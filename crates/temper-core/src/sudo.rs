//! One password per run: keep an already-granted sudo timestamp warm.
//!
//! sudo caches credentials per-tty for `timestamp_timeout` minutes (5 by default
//! on macOS). A converge that installs pkg-based casks (`dotnet-sdk`, `mactex`,
//! `zoom`, …) needs root once per cask, and the multi-GB downloads between them
//! blow past that window — so Homebrew re-prompts for *every one of them*, at
//! unpredictable points in a long unattended run. Homebrew shells out to plain
//! `/usr/bin/sudo` with no keep-alive of its own, and `brew bundle` exposes no
//! way to pass one in, so only temper — the parent process, sharing the same tty
//! and therefore the same timestamp record — is in a position to hold it open.
//!
//! temper never asks for a password itself. The refresh is `sudo -n -v`, which
//! extends an *existing* timestamp and silently does nothing when there is none
//! (`-n` = never prompt). So the first prompt still comes from brew, in brew's
//! own words; this only stops the second through the eighth. That also keeps the
//! whole thing inert on a machine that never needs root: nothing to extend, no
//! prompt, no output.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::primitives::which;

/// How often to re-extend the timestamp. Comfortably inside the 5-minute default
/// so a slow download can't strand the run between two refreshes.
const REFRESH: Duration = Duration::from_secs(60);

/// Tick granularity — the thread wakes this often to notice a stop request, so
/// dropping the guard doesn't block for up to `REFRESH`.
const TICK: Duration = Duration::from_secs(1);

/// A live keep-alive. Refreshing stops when this is dropped, and the timestamp
/// then expires on sudo's own schedule (temper does not `sudo -k`: the user may
/// have had a valid timestamp before temper ran, and revoking it is not
/// temper's call).
pub struct KeepAlive {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// Start keeping the sudo timestamp warm for as long as the returned guard
/// lives. A no-op (returning an inert guard) when there is no `sudo`, when stdin
/// isn't a terminal — an unattended run has no password to cache, and a stray
/// `sudo -n -v` there is pointless — or when `TEMPER_NO_SUDO_KEEPALIVE` is set.
pub fn keep_alive() -> KeepAlive {
    let inert = KeepAlive {
        stop: Arc::new(AtomicBool::new(true)),
        handle: None,
    };
    if std::env::var_os("TEMPER_NO_SUDO_KEEPALIVE").is_some() || which("sudo").is_none() {
        return inert;
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return inert;
    }

    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        // Refresh straight away, then every REFRESH: whichever child process
        // prompted, we want to extend it as soon as it exists rather than
        // waiting out a full interval first.
        loop {
            refresh();
            let mut waited = Duration::ZERO;
            while waited < REFRESH {
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(TICK);
                waited += TICK;
            }
        }
    });
    KeepAlive {
        stop,
        handle: Some(handle),
    }
}

/// Whether a usable sudo timestamp already exists (never prompts).
pub fn cached() -> bool {
    which("sudo").is_some()
        && Command::new("sudo")
            .args(["-n", "-v"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

/// Take the password **once, up front**, having established that this run needs
/// root — `reason` says what for, in temper's words, before the prompt appears.
///
/// Why temper asks rather than letting Homebrew ask: brew prompts at the moment
/// it reaches each pkg-based cask, which is minutes-to-hours into an unattended
/// run and interleaved with temper's own progress rendering. Asking here means
/// one prompt, at the keyboard, before any download starts — and combined with
/// [`keep_alive`] it is the only prompt the run needs.
///
/// Returns whether a timestamp is now held. `false` (no `sudo`, no tty, or a
/// refused/failed prompt) is not fatal: the run continues exactly as it did
/// before, with Homebrew prompting for itself when it gets there.
pub fn acquire(reason: &str) -> bool {
    if which("sudo").is_none() {
        return false;
    }
    if cached() {
        return true; // already valid — don't ask for what we have
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return false; // unattended: nothing to type a password into
    }
    // stderr, so `--json` stays pipe-clean.
    eprintln!("{} {reason}", crate::ui::cyan("→"));
    Command::new("sudo")
        .arg("-v")
        .status() // inherits the tty: sudo's own prompt, typed into directly
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Extend an existing timestamp. `-n` guarantees this never prompts: with no
/// cached credentials it just exits non-zero, which is the "nothing to keep
/// alive yet" case and not an error.
fn refresh() {
    let _ = Command::new("sudo")
        .args(["-n", "-v"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

impl Drop for KeepAlive {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_out_is_inert() {
        // Belt-and-braces: the sandboxed test runner has no tty either, so this
        // asserts the explicit opt-out specifically.
        std::env::set_var("TEMPER_NO_SUDO_KEEPALIVE", "1");
        let k = keep_alive();
        assert!(k.handle.is_none(), "opt-out must not spawn a thread");
        std::env::remove_var("TEMPER_NO_SUDO_KEEPALIVE");
    }

    #[test]
    fn drop_is_prompt_and_does_not_hang() {
        // An inert guard (no tty under `cargo test`) must still drop cleanly.
        let k = keep_alive();
        drop(k);
    }
}
