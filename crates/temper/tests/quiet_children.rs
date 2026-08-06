//! Proves the output contract for the package-manager children: a tool's own
//! chatter never reaches temper's stdout, so it can neither masquerade as
//! temper's verdict nor corrupt `--json`.
//!
//! The regression this locks: `flatpak update` prints `Nothing to update.`
//! whenever its remotes carry nothing new. Run with inherited stdio (as it was),
//! that line landed mid-converge — in flatpak's voice, about flatpak's remotes —
//! reading as "this run has nothing to do" immediately before temper installed
//! and upgraded plenty. On `--json` it also broke the parse outright.
//!
//! Hermetic: `PATH` is a temp dir holding one stub `flatpak`, so no real package
//! manager (brew included — it's simply absent from that PATH) can be reached.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;

fn os() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

/// A `flatpak` that answers the probe and prints the real tool's no-op verdict.
/// Every invocation is logged to `$STUB_LOG` so a test can prove the stub really
/// ran — an assertion about absent output is worthless if the child never fired.
fn stub_flatpak(dir: &Path) {
    let bin = dir.join("flatpak");
    fs::write(
        &bin,
        "#!/bin/sh\n\
         echo \"$@\" >> \"$STUB_LOG\"\n\
         # `list` is the probe — no apps installed.\n\
         case \"$1\" in\n\
         list) exit 0 ;;\n\
         update) echo 'Looking for updates…'; echo 'Nothing to update.'; exit 0 ;;\n\
         *) exit 0 ;;\n\
         esac\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A home declaring one flatpak (so the upgrade phase is not skipped as "no
/// packages declared") and no config steps.
fn home_with_one_flatpak() -> TempDir {
    let home = TempDir::new().unwrap();
    fs::write(
        home.path().join("temper.toml"),
        format!(
            "[[machine]]\nname = \"test\"\nos = \"{}\"\npackages = ['flatpak \"org.x.App\"']\n",
            os()
        ),
    )
    .unwrap();
    home
}

fn temper(home: &Path, fake_home: &Path, state: &Path, stub_dir: &Path, log: &Path) -> Command {
    let mut c = Command::cargo_bin("temper").unwrap();
    c.env("TEMPER_DIR", home)
        .env("HOME", fake_home)
        .env("TEMPER_STATE_DIR", state)
        .env("STUB_LOG", log)
        // Only the stub is reachable: brew/mas/gext/rpm-ostree are absent, so
        // every other provider stays a guarded no-op.
        .env("PATH", stub_dir);
    c
}

#[test]
fn child_chatter_never_reaches_stdout() {
    let home = home_with_one_flatpak();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let stub = TempDir::new().unwrap();
    stub_flatpak(stub.path());
    let log = stub.path().join("calls.log");

    let out = temper(
        home.path(),
        fake_home.path(),
        state.path(),
        stub.path(),
        &log,
    )
    .arg("update")
    .assert()
    .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();

    // The stub really ran — otherwise "no leak" proves nothing.
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.contains("update"),
        "the upgrade phase never invoked flatpak: {calls:?}"
    );
    assert!(
        !stdout.contains("Nothing to update."),
        "flatpak's verdict leaked into temper's stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains("Looking for updates"),
        "flatpak's progress leaked into temper's stdout: {stdout:?}"
    );
    // temper still speaks for itself.
    assert!(
        stdout.contains("update test:"),
        "temper's own summary is missing: {stdout:?}"
    );
}

#[test]
fn json_stays_parseable_through_a_chatty_child() {
    let home = home_with_one_flatpak();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let stub = TempDir::new().unwrap();
    stub_flatpak(stub.path());
    let log = stub.path().join("calls.log");

    let out = temper(
        home.path(),
        fake_home.path(),
        state.path(),
        stub.path(),
        &log,
    )
    .args(["--json", "update"])
    .assert()
    .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        fs::read_to_string(&log).unwrap_or_default().contains("update"),
        "the upgrade phase never invoked flatpak"
    );

    // The whole point of the capture: stdout is exactly one JSON document.
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout:?}"));
    assert_eq!(v["machine"], "test");
}
