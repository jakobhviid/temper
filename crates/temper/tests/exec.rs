//! Proves the `exec` primitive: check-gated run, drift-hook, secret
//! passthrough, and loud failure on a missing secret — all in temp dirs.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use tempfile::TempDir;

fn os() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

fn temper(home: &Path, fake_home: &Path, state: &Path) -> Command {
    let mut c = Command::cargo_bin("temper").unwrap();
    c.env("TEMPER_DIR", home)
        .env("HOME", fake_home)
        .env("TEMPER_STATE_DIR", state);
    c
}

#[test]
fn exec_check_secret_and_failure() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("scripts")).unwrap();
    // setup writes the secret into a marker file under $HOME
    fs::write(
        h.join("scripts/setup.sh"),
        "echo \"$MY_SECRET\" > \"$HOME/.exec-ran\"\n",
    )
    .unwrap();
    // check passes once the marker file exists
    fs::write(h.join("scripts/check.sh"), "test -f \"$HOME/.exec-ran\"\n").unwrap();
    fs::write(
        h.join("temper.toml"),
        format!(
            "[[machine]]\nname = \"test\"\nos = \"{}\"\napps = [\"demo\"]\n",
            os()
        ),
    )
    .unwrap();
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\nexec = \"scripts/setup.sh\"\ncheck = \"scripts/check.sh\"\nsecrets = [\"MY_SECRET\"]\n",
    )
    .unwrap();

    let marker = fake_home.path().join(".exec-ran");

    // install → check fails (no marker), script runs with the secret
    temper(h, fake_home.path(), state.path())
        .env("MY_SECRET", "hunter2")
        .arg("install")
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&marker).unwrap(), "hunter2\n");

    // drift → check passes, reported in sync
    temper(h, fake_home.path(), state.path())
        .env("MY_SECRET", "hunter2")
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("in sync"))
        .stdout(predicates::str::contains("0 out of sync"));

    // re-install with a DIFFERENT secret → check already passes, so the script
    // is skipped (idempotent); the marker keeps its original content.
    temper(h, fake_home.path(), state.path())
        .env("MY_SECRET", "changed")
        .arg("install")
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "hunter2\n",
        "exec re-ran despite passing check"
    );

    // drift with the secret UNSET → read-only must NOT abort; the exec check
    // degrades to status-only ("unavailable — secret …"), 0 out of sync.
    temper(h, fake_home.path(), state.path())
        .env_remove("MY_SECRET")
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("unavailable"))
        .stdout(predicates::str::contains("0 out of sync"));

    // install --dry-run with the secret unset → also read-only, must not abort.
    temper(h, fake_home.path(), state.path())
        .env_remove("MY_SECRET")
        .args(["install", "--dry-run"])
        .assert()
        .success();

    // remove the marker (check now fails) and drop the secret → a real install
    // must still fail loudly rather than run without the required secret.
    fs::remove_file(&marker).unwrap();
    temper(h, fake_home.path(), state.path())
        .env_remove("MY_SECRET")
        .arg("install")
        .assert()
        .failure()
        .stderr(predicates::str::contains("MY_SECRET"));
}

/// exec output is quiet by default (its chatter must not masquerade as temper's
/// own reporting), streamed under `--verbose`, and replayed on failure so the
/// error stays debuggable.
#[test]
fn exec_output_is_quiet_unless_verbose_or_failing() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("scripts")).unwrap();
    // No check hook → runs on every install; echoes a distinctive marker.
    fs::write(h.join("scripts/chatty.sh"), "echo NOTHING_TO_UPDATE\n").unwrap();
    fs::write(
        h.join("temper.toml"),
        format!(
            "[[machine]]\nname = \"test\"\nos = \"{}\"\napps = [\"demo\"]\n",
            os()
        ),
    )
    .unwrap();
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\nexec = \"scripts/chatty.sh\"\n",
    )
    .unwrap();

    // quiet by default → the script's chatter is captured, not shown.
    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success()
        .stdout(predicates::str::contains("NOTHING_TO_UPDATE").not());

    // --verbose → the script streams live.
    temper(h, fake_home.path(), state.path())
        .args(["install", "--verbose"])
        .assert()
        .success()
        .stdout(predicates::str::contains("NOTHING_TO_UPDATE"));

    // A failing exec replays its captured output even when quiet, so the error
    // is debuggable, and the run fails loudly — on **stderr**. A failing
    // script's stdout is diagnostic output about the failure, not temper's
    // answer to the command, and putting it on stdout meant a failing `install
    // --json` emitted a script's chatter where the document belonged.
    fs::write(
        h.join("scripts/chatty.sh"),
        "echo BOOM_DETAIL\nexit 3\n",
    )
    .unwrap();
    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .failure()
        .stderr(predicates::str::contains("BOOM_DETAIL"))
        .stdout(predicates::str::contains("BOOM_DETAIL").not());
}

/// A captured script that runs long must say which step the run is waiting on.
///
/// The step phase clears its progress region while an `exec` runs — the script may
/// prompt on the tty (`sudo`/polkit/PAM), and a live region drawn over that prompt
/// both hides the message the run is blocked on and leaves the half-drawn line
/// behind. With the region stood down, a slow script would otherwise be a silent
/// terminal, so temper names it after a few seconds.
#[test]
fn a_slow_exec_says_what_it_is_waiting_on() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();
    // Longer than the notice threshold, and noisy — the chatter must stay hidden
    // even though the notice appears.
    fs::write(h.join("assets/slow.sh"), "sleep 4\necho chatter\n").unwrap();
    fs::write(
        h.join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"demo\"]\n", os()),
    )
    .unwrap();
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\nexec = \"assets/slow.sh\"\nrun = \"always\"\n",
    )
    .unwrap();

    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success()
        // A subordinate detail line that says what it is — deliberately NOT a row
        // of the results list, whose leading glyph is a status column.
        .stderr(predicates::str::contains("still working: assets/slow.sh"))
        // Quiet-on-success still holds for the script's own output.
        .stdout(predicates::str::contains("chatter").not());
}

/// AGENTS.md question 7: a run whose changes cannot be reverted has to say so
/// **before** you commit to it, not in the report afterwards. `--dry-run` is the
/// preview, so the limit belongs there — and it belongs there only for steps
/// that would actually change something, since warning about a converged
/// `sysfile` warns about work that is not going to happen.
#[test]
fn a_dry_run_names_what_undo_could_not_revert() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();
    fs::write(h.join("assets/x.sh"), "true\n").unwrap();
    fs::write(h.join("assets/x.conf"), "owned by temper\n").unwrap();
    fs::write(
        h.join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"demo\"]\n", os()),
    )
    .unwrap();
    // One unrevertible step (`exec`) and one journaled one (`copy`), both of
    // which would change something on this untouched machine.
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\nexec = \"assets/x.sh\"\nrun = \"always\"\n\n\
         [[step]]\ncopy = \"assets/x.conf\"\nto = \"~/x.conf\"\n",
    )
    .unwrap();

    let out = temper(h, fake_home.path(), state.path())
        .args(["install", "--dry-run", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let un = v["unrevertible"].as_array().expect("unrevertible in --json");
    assert_eq!(
        un.len(),
        1,
        "the exec should be named and the copy should not: {un:?}"
    );
    assert!(
        un[0].as_str().unwrap().contains("assets/x.sh"),
        "the reason must name the step, got {:?}",
        un[0]
    );
    // The dry run wrote nothing, so the forecast has to read like one.
    temper(h, fake_home.path(), state.path())
        .args(["install", "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains("would not be able to revert"));
    assert!(
        !fake_home.path().join("x.conf").exists(),
        "a dry run must not deploy anything"
    );
}

/// The reason is a property of the primitive, so it is stated **once** however
/// many steps share it.
///
/// It used to be joined onto every row: three `exec` steps printed the same
/// sentence three times, and each step's own name sat to the left of it where
/// the eye had to hunt for it. Three affected steps read as three different
/// problems. The rows are terse now and the explanation is a legend beneath
/// them — the shape `grove` uses for a multi-repo table.
#[test]
fn one_reason_is_printed_once_however_many_steps_share_it() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();
    for n in ["one", "two", "three"] {
        fs::write(h.join(format!("assets/{n}.sh")), "true\n").unwrap();
    }
    fs::write(
        h.join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"demo\"]\n", os()),
    )
    .unwrap();
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\nexec = \"assets/one.sh\"\nrun = \"always\"\n\n\
         [[step]]\nexec = \"assets/two.sh\"\nrun = \"always\"\n\n\
         [[step]]\nexec = \"assets/three.sh\"\nrun = \"always\"\n",
    )
    .unwrap();

    let out = temper(h, fake_home.path(), state.path())
        .args(["install", "--dry-run"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);

    let reason = "runs arbitrary code";
    assert_eq!(
        text.matches(reason).count(),
        1,
        "three execs share one reason, so it belongs on one line:\n{text}"
    );
    // …and every step is still named, or the legend saved space by losing the
    // thing the reader actually needs.
    for n in ["one.sh", "two.sh", "three.sh"] {
        assert!(text.contains(n), "step `{n}` is not named:\n{text}");
    }
    // A blank line separates the rows from the legend. Dimmed and outdented was
    // not enough on its own: sitting directly under the last row, the legend
    // read as one more row, which is the confusion the legend exists to remove.
    let lines: Vec<&str> = text.lines().collect();
    let legend = lines
        .iter()
        .position(|l| l.contains(reason))
        .expect("the legend line");
    assert!(
        legend > 0 && lines[legend - 1].trim().is_empty(),
        "the legend must be separated from the rows by a blank line:\n{text}"
    );
}

/// Two different primitives are two different reasons, so both are printed —
/// deduplication must not collapse them into one.
#[test]
fn two_different_reasons_are_both_printed() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();
    fs::write(h.join("assets/x.sh"), "true\n").unwrap();
    fs::write(h.join("assets/f.conf"), "x\n").unwrap();
    fs::write(
        h.join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"demo\"]\n", os()),
    )
    .unwrap();
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\nexec = \"assets/x.sh\"\nrun = \"always\"\n\n\
         [[step]]\nsysfile = \"assets/f.conf\"\nto = \"/tmp/temper-test-absent/f.conf\"\n",
    )
    .unwrap();

    let out = temper(h, fake_home.path(), state.path())
        .args(["install", "--dry-run"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("runs arbitrary code") && text.contains("outside the journal"),
        "each distinct reason needs its own line:\n{text}"
    );
}
