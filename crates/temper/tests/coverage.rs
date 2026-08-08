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
