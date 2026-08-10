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

/// No output glyph may be one a colour-emoji font covers.
///
/// Those codepoints (`ℹ` U+2139, `⚠` U+26A0, …) have a colour glyph in fonts like
/// Noto Color Emoji, and terminals prefer that font — so they render
/// DOUBLE-WIDTH. That swallows the space after them (`ⓘa system update is
/// staged`) and, worse, silently breaks `ui::Columns`, which measures alignment
/// in characters. `✓`/`✗`/`→`/`ⓘ` have no colour glyph anywhere, so they always
/// come from the text font at one cell.
#[test]
fn no_output_glyph_renders_double_width() {
    // Emoji=Yes codepoints that are otherwise tempting as terminal glyphs.
    // Status markers now come from `ui::g_*`, which switches on `[ui].icons` —
    // so a literal glyph at a call site is itself the bug: it can't follow the
    // setting. This list guards the Unicode set's *choices*; legibility (which
    // is why `ⓘ` was rejected) is judgement no test can make, and is written
    // down in ui.rs instead.
    //
    // `⚠` is deliberately NOT in this list. It is emoji-covered too, but it is
    // the established warning glyph and renders acceptably in practice; the
    // observed breakage was `ℹ`, whose colour glyph is far more widely shipped.
    // If a warning ever mashes against its text, the fix is U+FE0E (text
    // presentation selector) rather than a different symbol.
    const EMOJI_COVERED: &[char] = &['ℹ', '❗', '❓', '✅', '❌', '⏳', '⌛', '⭐', '☑'];
    // Every source that prints, not just the CLI: `ui`, `sudo` and `plan` all
    // emit their own lines.
    let sources = [
        ("temper/src/main.rs", include_str!("../src/main.rs")),
        ("temper-core/src/ui.rs", include_str!("../../temper-core/src/ui.rs")),
        ("temper-core/src/sudo.rs", include_str!("../../temper-core/src/sudo.rs")),
        ("temper-core/src/plan.rs", include_str!("../../temper-core/src/plan.rs")),
        ("temper-core/src/git.rs", include_str!("../../temper-core/src/git.rs")),
    ];
    for (name, src) in sources {
    for (n, line) in src.lines().enumerate() {
        // Only the lines that actually print a glyph.
        if !line.contains("ui::") {
            continue;
        }
        for c in EMOJI_COVERED {
            assert!(
                !line.contains(*c),
                "{name}:{} uses `{c}`, which a colour-emoji font covers — it renders \
                 double-width, eats the following space, and breaks column alignment. \
                 Use a text-only glyph (ⓘ ↻ ⧗ ✓ ✗ →).",
                n + 1
            );
        }
    }
    }
}

/// Every candidate list a `ReconcilePlan` carries must reach BOTH aggregation
/// points in `cmd_reconcile`: the `--json` plan document, and the emptiness
/// check that decides whether there is anything to do at all.
///
/// This is the test that would have caught `gext_adds`. It was wired into the
/// prompt loop, the `--csw` path, the selection check and the counts — four
/// sites — and missed the two that matter most. Missing from `--json` meant a
/// consumer previewed a reconcile that then changed something it was never
/// shown; missing from the emptiness check made the whole feature UNREACHABLE
/// whenever an undeclared extension was the only drift, behind a note claiming
/// reconcile could not absorb them.
///
/// A field-per-kind plan struct invites exactly this: N fields, ~10 aggregation
/// points, nothing relating them. Until the plan is one homogeneous item list,
/// a source scrape is the honest enforcement.
#[test]
fn every_reconcile_plan_field_reaches_json_and_the_emptiness_check() {
    let plan_src = include_str!("../../temper-core/src/reconcile.rs");
    let cli_src = include_str!("../src/main.rs");

    // Candidate lists declared on `ReconcilePlan` (skip `brewfile_rel`, which is
    // a location, not a candidate).
    let struct_body = {
        let start = plan_src
            .find("pub struct ReconcilePlan {")
            .expect("ReconcilePlan struct");
        let rest = &plan_src[start..];
        &rest[..rest.find("\n}").expect("struct end")]
    };
    let fields: Vec<&str> = struct_body
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub "))
        .filter_map(|l| l.split_once(':'))
        .map(|(name, _)| name)
        .filter(|n| *n != "brewfile_rel")
        .collect();
    assert!(fields.len() >= 6, "scrape found too few fields: {fields:?}");

    let body = {
        let start = cli_src.find("fn cmd_reconcile(").expect("cmd_reconcile");
        &cli_src[start..]
    };
    // The `--json` plan document, and the emptiness check just after it.
    let json_doc = {
        let s = body.find("if json && !(csw && yes)").expect("json branch");
        let rest = &body[s..];
        &rest[..rest.find("return Ok(());").expect("json branch end")]
    };
    let emptiness = {
        let s = body.find("if plan.adds.is_empty()").expect("emptiness check");
        let rest = &body[s..];
        &rest[..rest.find("    {").expect("emptiness end")]
    };

    for f in &fields {
        assert!(
            json_doc.contains(f),
            "ReconcilePlan.{f} never reaches the --json plan document — a consumer \
             would preview a reconcile that changes something it was not shown"
        );
        assert!(
            emptiness.contains(f),
            "ReconcilePlan.{f} is missing from the emptiness check — its feature is \
             UNREACHABLE whenever it is the only drift present"
        );
    }
}

/// Every verb that writes the temper folder must fire `after_repo_change`.
///
/// A folder-writing verb that skips it leaves a git-backed home silently dirty.
/// `init` did (it delegated to a `reconcile` that returned early), then `undo`
/// did — the one command whose whole job is putting things back — and then
/// `configure set|unset`. Three instances of one omission, none of which any
/// test could see.
#[test]
fn every_folder_writing_verb_fires_the_repo_hook() {
    let cli_src = include_str!("../src/main.rs");
    for verb in [
        "cmd_reconcile",
        "cmd_init",
        "cmd_configure",
        "cmd_undo",
        "cmd_snapshot",
        "cmd_eq_import",
    ] {
        let Some(start) = cli_src.find(&format!("fn {verb}(")) else {
            continue; // renamed or removed — the verb-name test covers that
        };
        let rest = &cli_src[start..];
        // Function body ends at the next top-level `\n}` .
        let body = &rest[..rest.find("\n}\n").unwrap_or(rest.len())];
        assert!(
            body.contains("after_repo_change"),
            "{verb} writes the folder but never calls after_repo_change — a \
             git-backed home is left silently dirty"
        );
    }
}
