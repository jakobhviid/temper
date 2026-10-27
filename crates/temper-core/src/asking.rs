//! Which of temper's own subprocesses is waiting for the user to type.
//!
//! `sudo`, polkit's agents, `ssh` and PAM ask on `/dev/tty`, which none of the
//! pipes temper reads capture, so a question lands under a live progress region
//! and the next redraw erases it. This finds the asker, so the region can stop
//! redrawing and say who is asking and for what — see `ui::watch_for_askers`.
//!
//! Two signals, because the usual asker cannot be inspected:
//!
//! - **A `sudo` (or `doas`, `su`, `pkexec`) that has not started its command.**
//!   sudo runs as root, so its file descriptors and system call are closed to us —
//!   but its process entry is not, and a sudo that authenticated starts its
//!   command within milliseconds. One still childless is asking. `-n` never asks,
//!   so a sudo carrying it is not one (temper's own keep-alive passes it).
//! - **A process blocked reading a terminal.** Anything running as the user is
//!   open to us: on Linux `/proc/<pid>/syscall` names the call it is blocked in
//!   and the descriptor it reads, and that descriptor's link says whether it is a
//!   terminal. temper's children read pipes or nothing, so one reading a terminal
//!   is asking the person at it. macOS reports the same wait as `ttyin`.

use std::collections::HashMap;

/// A subprocess waiting for the user, and the sentence that says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asker {
    pub pid: u32,
    pub what: String,
    /// The answer is a password, which the terminal does not echo.
    pub password: bool,
}

/// The first descendant of this process that is waiting for the user, if any.
pub fn find() -> Option<Asker> {
    find_under(std::process::id())
}

/// The same, under any process — so a test can watch a subtree of its own.
pub fn find_under(root: u32) -> Option<Asker> {
    let procs = snapshot(root)?;
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs.values() {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut stack: Vec<u32> = children.get(&root).cloned().unwrap_or_default();
    while let Some(pid) = stack.pop() {
        let Some(p) = procs.get(&pid) else { continue };
        let kids = children.get(&pid).cloned().unwrap_or_default();
        if let Some(what) = describe(p, kids.is_empty()) {
            let password = SUDO_LIKE.contains(&p.name()) || POLKIT.contains(&p.name());
            return Some(Asker { pid, what, password });
        }
        stack.extend(kids);
    }
    None
}

/// The programs that ask for a password before running something as root.
const SUDO_LIKE: &[&str] = &["sudo", "doas", "su", "pkexec", "run0"];
/// polkit's terminal agents, which ask for a password on another program's behalf.
const POLKIT: &[&str] = &["pkttyagent", "polkit-agent-helper-1"];

/// One process, as much as the platform lets us see.
#[derive(Debug, Clone)]
struct Proc {
    pid: u32,
    ppid: u32,
    /// argv, as the process was started.
    args: Vec<String>,
    /// Blocked reading a terminal. `None` when that cannot be told — a root-owned
    /// process on Linux.
    reads_tty: Option<bool>,
}

impl Proc {
    fn name(&self) -> &str {
        self.args
            .first()
            .map(|a| a.rsplit('/').next().unwrap_or(a))
            .unwrap_or("")
    }
}

/// What makes `p` an asker, said in a line — or `None`.
fn describe(p: &Proc, childless: bool) -> Option<String> {
    let name = p.name();
    let rest = command_of(&p.args);
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "your".into());
    let to_run = |s: String| {
        if rest.is_empty() {
            s
        } else {
            format!("{s}, to run `{}`", shorten(&rest))
        }
    };
    match name {
        _ if SUDO_LIKE.contains(&name) => {
            let never_asks = p.args.iter().any(|a| a == "-n" || a == "--non-interactive");
            if !childless || never_asks {
                return None;
            }
            Some(match name {
                // su asks for the password of the account it switches to.
                "su" => to_run("su is asking for root's password".into()),
                "pkexec" | "run0" => to_run(format!("{name} is asking for a password")),
                _ => to_run(format!("{name} is asking for {user}'s password")),
            })
        }
        _ if p.reads_tty == Some(true) => Some(match name {
            _ if POLKIT.contains(&name) => "polkit is asking for a password".into(),
            _ => format!("`{}` is waiting for an answer", shorten(&p.args.join(" "))),
        }),
        _ => None,
    }
}

/// The command a sudo-like runs: its arguments after its own options.
fn command_of(args: &[String]) -> String {
    // The options that take a value, so the value is not mistaken for the command.
    const WITH_VALUE: &[&str] = &["-u", "-g", "-p", "-C", "-D", "-h", "-r", "-t", "-U", "-c"];
    let mut it = args.iter().skip(1);
    let mut out: Vec<&str> = Vec::new();
    while let Some(a) = it.next() {
        if !out.is_empty() {
            out.push(a);
        } else if a == "--" {
            out.extend(it.by_ref().map(String::as_str));
        } else if WITH_VALUE.contains(&a.as_str()) {
            it.next();
        } else if !a.starts_with('-') {
            out.push(a);
        }
    }
    out.join(" ")
}

/// A command short enough for one line.
fn shorten(s: &str) -> String {
    const MAX: usize = 60;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(MAX - 1).collect::<String>())
    }
}

/// The processes under `root`. Walked down from it through each thread's
/// `children` list, so a look costs what temper is running rather than what the
/// machine is — every 60 ms while a region draws, that difference is the point.
/// A kernel without those lists gets the whole of `/proc` instead.
#[cfg(target_os = "linux")]
fn snapshot(root: u32) -> Option<HashMap<u32, Proc>> {
    let mut out = HashMap::new();
    let Some(mut stack) = children_of(root) else {
        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
            if let Some(p) = entry.file_name().to_str().and_then(|s| s.parse().ok()).and_then(read_proc) {
                out.insert(p.pid, p);
            }
        }
        return Some(out);
    };
    while let Some(pid) = stack.pop() {
        if let Some(p) = read_proc(pid) {
            stack.extend(children_of(pid).unwrap_or_default());
            out.insert(pid, p);
        }
    }
    Some(out)
}

/// A process's children, from every one of its threads — a child belongs to the
/// thread that started it. `None` when the kernel keeps no such list.
#[cfg(target_os = "linux")]
fn children_of(pid: u32) -> Option<Vec<u32>> {
    let mut out = Vec::new();
    for task in std::fs::read_dir(format!("/proc/{pid}/task")).ok()?.flatten() {
        let list = std::fs::read_to_string(task.path().join("children")).ok()?;
        out.extend(list.split_whitespace().filter_map(|c| c.parse::<u32>().ok()));
    }
    Some(out)
}

#[cfg(target_os = "linux")]
fn read_proc(pid: u32) -> Option<Proc> {
    let base = std::path::PathBuf::from(format!("/proc/{pid}"));
    let stat = std::fs::read_to_string(base.join("stat")).ok()?;
    // The command name is parenthesised and may contain spaces; ppid is the second
    // field after it.
    let after = &stat[stat.rfind(')')? + 1..];
    let ppid = after.split_whitespace().nth(1)?.parse().ok()?;
    let args: Vec<String> = std::fs::read(base.join("cmdline"))
        .unwrap_or_default()
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    let reads_tty = reads_tty(&base);
    Some(Proc { pid, ppid, args, reads_tty })
}

/// `/proc/<pid>/syscall` names the call a process is blocked in and its
/// arguments; a `read` whose descriptor links to a terminal is a question being
/// waited on. Unreadable — a process running as someone else — is `None`.
#[cfg(target_os = "linux")]
fn reads_tty(base: &std::path::Path) -> Option<bool> {
    #[cfg(target_arch = "x86_64")]
    const READ: &str = "0";
    #[cfg(target_arch = "aarch64")]
    const READ: &str = "63";
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    return None;
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let call = std::fs::read_to_string(base.join("syscall")).ok()?;
        let mut f = call.split_whitespace();
        if f.next()? != READ {
            return Some(false);
        }
        let fd = u64::from_str_radix(f.next()?.trim_start_matches("0x"), 16).ok()?;
        let target = std::fs::read_link(base.join("fd").join(fd.to_string())).ok()?;
        let t = target.to_string_lossy();
        Some(t.starts_with("/dev/pts/") || t.starts_with("/dev/tty"))
    }
}

/// `ps`, because macOS has no `/proc`. `ttyin` is the wait channel of a process
/// blocked reading its terminal. Not yet confirmed on a Mac — ROADMAP.
#[cfg(target_os = "macos")]
fn snapshot(_root: u32) -> Option<HashMap<u32, Proc>> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,wchan=,args="])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let mut procs = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut f = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(wchan)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else { continue };
        let args: Vec<String> = f.map(str::to_string).collect();
        procs.insert(pid, Proc { pid, ppid, args, reads_tty: Some(wchan == "ttyin") });
    }
    Some(procs)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn snapshot(_root: u32) -> Option<HashMap<u32, Proc>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(args: &[&str], reads_tty: Option<bool>) -> Proc {
        Proc {
            pid: 1,
            ppid: 0,
            args: args.iter().map(|s| s.to_string()).collect(),
            reads_tty,
        }
    }

    #[test]
    fn a_sudo_that_has_not_started_its_command_is_asking() {
        std::env::set_var("USER", "jakob");
        let p = proc(&["/usr/bin/sudo", "-u", "root", "installer", "-pkg", "zoom.pkg"], None);
        assert_eq!(
            describe(&p, true).unwrap(),
            "sudo is asking for jakob's password, to run `installer -pkg zoom.pkg`"
        );
        // Started its command: authenticated, not asking.
        assert_eq!(describe(&p, false), None);
        // `-n` never asks — temper's own keep-alive.
        assert_eq!(describe(&proc(&["sudo", "-n", "-v"], None), true), None);
        assert_eq!(
            describe(&proc(&["su", "-c", "true"], None), true).unwrap(),
            "su is asking for root's password"
        );
    }

    #[test]
    fn a_process_reading_the_terminal_is_waiting_for_an_answer() {
        let ssh = proc(&["ssh", "git@github.com", "git-upload-pack"], Some(true));
        assert_eq!(
            describe(&ssh, true).unwrap(),
            "`ssh git@github.com git-upload-pack` is waiting for an answer"
        );
        assert_eq!(
            describe(&proc(&["/usr/bin/pkttyagent", "--process", "42"], Some(true)), true).unwrap(),
            "polkit is asking for a password"
        );
        assert_eq!(describe(&proc(&["brew", "install"], Some(false)), true), None);
        assert_eq!(describe(&proc(&["brew", "install"], None), true), None);
    }

    #[test]
    fn a_sudo_like_command_is_what_follows_its_options() {
        let args = |s: &[&str]| s.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert_eq!(command_of(&args(&["sudo", "-E", "-u", "root", "dnf", "-y", "install", "x"])), "dnf -y install x");
        assert_eq!(command_of(&args(&["sudo", "--", "-weird"])), "-weird");
        assert_eq!(command_of(&args(&["sudo", "-v"])), "");
    }

    /// Against real processes: a shell blocked on `read` in a terminal of its own
    /// (`script` gives it one), and a childless program named `sudo`, are found;
    /// a sleeping child is not.
    #[cfg(target_os = "linux")]
    #[test]
    fn real_askers_are_found_in_a_subtree() {
        use std::process::{Command, Stdio};
        let spawn = |cmd: &mut Command| cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let settle = || std::thread::sleep(std::time::Duration::from_millis(400));

        let mut quiet = spawn(Command::new("sh").args(["-c", "sleep 5; true"]));
        settle();
        assert_eq!(find_under(quiet.id()), None);
        let _ = quiet.kill();
        let _ = quiet.wait();

        let dir = tempfile::tempdir().unwrap();
        let sudo = dir.path().join("sudo");
        std::fs::copy(crate::primitives::which("sleep").unwrap(), &sudo).unwrap();
        let mut asking = spawn(Command::new("sh").args(["-c", &format!("{} 5; true", sudo.display())]));
        settle();
        let a = find_under(asking.id()).expect("a childless sudo is asking");
        assert!(a.what.starts_with("sudo is asking for"), "{a:?}");
        let _ = asking.kill();
        let _ = asking.wait();

        if crate::primitives::which("script").is_some() {
            // stdin held open: `script` relays it, and end-of-input would answer the read.
            let mut reading = Command::new("script")
                .args(["-qec", "sh -c 'read x; true'", "/dev/null"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            settle();
            let a = find_under(reading.id());
            let _ = reading.kill();
            let _ = reading.wait();
            assert!(a.is_some_and(|a| a.what.contains("read x")), "a shell reading its terminal");
        }
    }
}

