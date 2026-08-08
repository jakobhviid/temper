//! Completeness checks over the (state × direction × verb) matrix.
//!
//! temper's recurring defect has not been broken code — it has been *missing
//! edges*: a report with no way to act on it, a direction with nowhere to write,
//! a remediation naming a verb that had been renamed. Each shipped green,
//! because nothing asserted the matrix was complete. These tests do.

use assert_cmd::Command;
use temper_core::plan;

/// Every command a remediation names must be a real, advertised verb.
///
/// This is the one that would have caught v3.2.0 shipping drift advice to run
/// `temper snapshot` — a verb renamed to `snapshot-gnome` in the same release.
/// It kept working (a hidden alias), so nothing failed; the only place whose job
/// is teaching the right command was teaching a dead one.
#[test]
fn every_remediation_names_a_real_verb() {
    let help = String::from_utf8(
        Command::cargo_bin("temper")
            .unwrap()
            .arg("--help")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();

    for cmd in plan::answer_commands() {
        let verb = cmd
            .strip_prefix("temper ")
            .unwrap_or_else(|| panic!("remediation `{cmd}` should start with `temper `"));
        let verb = verb.split_whitespace().next().unwrap();
        assert!(
            help.contains(&format!("\n  {verb} ")) || help.contains(&format!("\n  {verb}\n")),
            "remediation `{cmd}` names `{verb}`, which is not a verb `temper --help` \
             advertises — a rename left drift teaching a dead name"
        );
    }
}

/// A remediation must also be *runnable*: `--help` on it has to succeed. Catches
/// a command string whose flags drifted from the verb's real signature.
#[test]
fn every_remediation_command_actually_parses() {
    for cmd in plan::answer_commands() {
        let args: Vec<&str> = cmd.strip_prefix("temper ").unwrap().split(' ').collect();
        let mut c = Command::cargo_bin("temper").unwrap();
        c.args(&args).arg("--help");
        c.assert().success();
    }
}
