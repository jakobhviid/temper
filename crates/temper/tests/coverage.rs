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

/// Every candidate list a `ReconcilePlan` declares is enumerated by `items()`.
///
/// This used to scrape `main.rs` for each field, because emptiness, `--json`,
/// selection and counts were four hand-maintained aggregations related by
/// nothing but attention — and three providers in a row shipped with one or two
/// of them missed. Those four are now *derived* from `items()`, so there is one
/// place to forget instead of four, and this checks that one place.
///
/// Still a source scrape, and still the honest enforcement: Rust cannot ask a
/// struct for its fields at runtime. But the blast radius is one function in one
/// file rather than a CLI three thousand lines long.
#[test]
fn every_reconcile_plan_field_is_enumerated_by_items() {
    let src = include_str!("../../temper-core/src/reconcile.rs");

    let struct_body = {
        let start = src
            .find("pub struct ReconcilePlan {")
            .expect("ReconcilePlan struct");
        let rest = &src[start..];
        &rest[..rest.find("\n}").expect("struct end")]
    };
    let fields: Vec<&str> = struct_body
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub "))
        .filter_map(|l| l.split_once(':'))
        .map(|(name, _)| name)
        // A location, not a candidate.
        .filter(|n| *n != "brewfile_rel")
        .collect();
    assert!(fields.len() >= 10, "scrape found too few fields: {fields:?}");

    let items = {
        let start = src
            .find("pub fn items(&self) -> Vec<PlanItem> {")
            .expect("items()");
        let rest = &src[start..];
        &rest[..rest.find("\n    }").expect("items() end")]
    };
    for f in &fields {
        assert!(
            items.contains(&format!("self.{f}")),
            "ReconcilePlan.{f} is never enumerated by items() — it would be \
             invisible to the emptiness check, the --json document and the counts \
             all at once"
        );
    }
}

/// Every candidate list a `PrunePlan` declares is enumerated by `items()`.
///
/// The same guarantee `ReconcilePlan` has above, on the verb where it matters
/// more: `items()` feeds the count, the confirm ("remove N item(s) **listed
/// above**"), the preview and the `--json` document, so a field it misses is
/// deleted without ever being shown. That has happened twice — `len()` once
/// summed two of three lists and `prune` uninstalled three GNOME extensions
/// after asking to remove zero, and later `flatpak_remotes` and `retired` were
/// counted but never printed.
///
/// `residue_edited` is the one deliberate exception: it is **reported**, never
/// removed, so counting it would claim work that does not happen.
#[test]
fn every_prune_plan_field_is_enumerated_by_items() {
    let src = include_str!("../../temper-core/src/plan.rs");

    let struct_body = {
        let start = src.find("pub struct PrunePlan {").expect("PrunePlan struct");
        let rest = &src[start..];
        &rest[..rest.find("\n}").expect("struct end")]
    };
    let fields: Vec<&str> = struct_body
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub "))
        .filter_map(|l| l.split_once(':'))
        .map(|(name, _)| name.trim())
        // Reported, never removed — see above.
        .filter(|n| *n != "residue_edited")
        .collect();
    assert!(fields.len() >= 6, "scrape found too few fields: {fields:?}");

    let items = {
        let start = src
            .find("pub fn items(&self) -> Vec<(&'static str, String)> {")
            .expect("PrunePlan::items()");
        let rest = &src[start..];
        &rest[..rest.find("\n    }").expect("items() end")]
    };
    for f in &fields {
        assert!(
            items.contains(&format!("self.{f}")),
            "PrunePlan.{f} is never enumerated by items() — it would be counted \
             and removed without appearing in the preview, the confirm, or --json"
        );
    }

    // …and every label `items()` produces is named in `LISTS`, which is what the
    // preview and the `--json` document iterate.
    let lists = {
        let start = src
            .find("pub const LISTS: &'static [&'static str] = &[")
            .expect("PrunePlan::LISTS");
        let rest = &src[start..];
        &rest[..rest.find("];").expect("LISTS end")]
    };
    for (i, _) in items.match_indices("out.push((\"") {
        let rest = &items[i + "out.push((\"".len()..];
        let label = &rest[..rest.find('"').expect("label end")];
        assert!(
            lists.contains(label),
            "items() emits the label `{label}` but LISTS does not name it — the \
             preview and --json iterate LISTS, so it would be invisible in both"
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
        // A completeness test whose failure mode is "check fewer things" is the
        // defect this file exists to catch. The comment here used to say the
        // verb-name test covers a rename — it does not: that one checks CLI verb
        // strings against `--help`, not internal `cmd_*` function names. Rename
        // `cmd_snapshot` (as the CLI rename nearly did) and this quietly checked
        // five of six.
        let start = cli_src.find(&format!("fn {verb}(")).unwrap_or_else(|| {
            panic!(
                "`{verb}` is not in main.rs — if it was renamed, rename it here                  too; if it was removed, remove it here. Skipping it means this                  test silently stops covering a folder-writing verb."
            )
        });
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
