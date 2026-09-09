//! Every `temper <verb>` the docs print in backticks is a **canonical** verb.
//!
//! The docs compile into `temper --llm`, so they are how humans and agents both
//! learn what to run. When the snapshot/restore verbs were renamed, the aliases
//! kept every example working — so nothing failed, and the only documents whose
//! job is teaching the right command went on teaching the dead one, in twelve
//! places, across four files. `coverage.rs` already asserts that the commands
//! *drift* names are real; this is the same guarantee one level up, where
//! nothing was looking.
//!
//! An **alias** is deliberately not good enough. `temper snapshot-gnome` still
//! works and always will; teaching it is the defect.

use std::collections::BTreeSet;

use assert_cmd::Command;

/// The verbs `--help` lists — canonical names only, which is the point: aliases
/// are absent from that list by design.
fn canonical_verbs() -> BTreeSet<String> {
    let out = Command::cargo_bin("temper").unwrap().arg("--help").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut verbs = BTreeSet::new();
    let mut in_commands = false;
    for line in text.lines() {
        if line.starts_with("Commands:") {
            in_commands = true;
            continue;
        }
        if in_commands {
            if line.starts_with("Options:") || line.trim().is_empty() {
                if line.starts_with("Options:") {
                    break;
                }
                continue;
            }
            if let Some(word) = line.split_whitespace().next() {
                if word.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                    verbs.insert(word.to_string());
                }
            }
        }
    }
    assert!(verbs.len() > 10, "could not read the verb list from --help: {verbs:?}");
    verbs
}

/// A line that is *about* an old name rather than telling you to run it.
///
/// "`temper backup` is gone" and "`temper upgrade` is an alias for `temper
/// update`" are documentation of a rename; teaching the dead name as the thing
/// to type is the defect. The markers are deliberately few and specific — a
/// broad one would turn this test into a loophole rather than a guard.
fn explains_rather_than_instructs(line: &str) -> bool {
    ["alias", "is gone", "Used to run", "If you used"]
        .iter()
        .any(|m| line.contains(m))
}

/// Every `` `temper <word>` `` in a doc. Backticks only — prose like "temper
/// reads the folder" is not a command and must not be scanned as one.
fn commands_in(doc: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in doc.lines() {
        if explains_rather_than_instructs(line) {
            continue;
        }
        for (i, _) in line.match_indices("`temper ") {
            let rest = &line[i + "`temper ".len()..];
            let end = rest.find('`').unwrap_or(rest.len());
            if let Some(word) = rest[..end].split_whitespace().next() {
                if word.starts_with('-') || word.is_empty() {
                    continue; // `temper --llm`, `temper --version`
                }
                if word.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                    out.push(word.to_string());
                }
            }
        }
    }
    out
}

#[test]
fn every_documented_command_is_a_canonical_verb() {
    let verbs = canonical_verbs();
    // Every doc is checked. Nothing is exempt from naming the canonical verb —
    // the migration guide used to be, because printing an old spelling beside a
    // new one was its whole job, and it is gone.
    let docs: [(&str, &str); 6] = [
        ("WORKFLOWS.md", include_str!("../../../WORKFLOWS.md")),
        ("README.md", include_str!("../../../README.md")),
        ("ARCHITECTURE.md", include_str!("../../../ARCHITECTURE.md")),
        ("SPEC.md", include_str!("../../../SPEC.md")),
        ("PATTERNS.md", include_str!("../../../PATTERNS.md")),
        ("ROADMAP.md", include_str!("../../../ROADMAP.md")),
    ];

    let mut bad: Vec<String> = Vec::new();
    for (name, body) in docs {
        for cmd in commands_in(body) {
            if !verbs.contains(&cmd) {
                bad.push(format!("{name}: `temper {cmd}`"));
            }
        }
    }
    bad.sort();
    bad.dedup();
    assert!(
        bad.is_empty(),
        "these documents teach a command that is not a canonical verb — an alias \
         still works, which is exactly why nothing else catches this: {bad:#?}"
    );
}

/// Every doc in the repo is either compiled into `--llm` or explicitly exempt.
///
/// AGENTS.md: the operating documents "are **compiled into `temper --llm`**" —
/// that guide is how humans *and* LLMs learn to author a folder. A doc added
/// without an `include_str!` is invisible to every downstream agent, and nothing
/// would say so. The exemption list is short and each entry has a reason, so
/// adding a doc forces the choice rather than defaulting to "forgotten".
#[test]
fn every_doc_is_embedded_in_the_llm_guide_or_exempt() {
    let main_rs = include_str!("../src/main.rs");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root");

    // `--llm` teaches one job: OPERATE and AUTHOR a temper folder. A doc is
    // exempt when its reader is doing something else, and each exemption has to
    // name who that reader is — otherwise "exempt" becomes the place documents go
    // to stop being maintained.
    //
    //   AGENTS.md / CLAUDE.md  people and agents working on temper's own source.
    //   INTERNALS.md           the same reader: how the tool is built, what it
    //                          would take to extend it. Someone writing
    //                          `apps/ghostty.toml` never needs it.
    let exempt = ["AGENTS.md", "CLAUDE.md", "INTERNALS.md"];

    let mut missing = Vec::new();
    let mut seen = 0;
    for entry in std::fs::read_dir(&root).expect("read repo root") {
        let path = entry.expect("dir entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".md") {
            continue;
        }
        seen += 1;
        if exempt.contains(&name) {
            continue;
        }
        if !main_rs.contains(&format!("{name}\")")) {
            missing.push(name.to_string());
        }
    }
    assert!(seen >= 8, "only found {seen} docs — the scan is not seeing the root");
    missing.sort();
    assert!(
        missing.is_empty(),
        "these docs are not compiled into `temper --llm`, so nothing reading that \
         guide can see them — embed them, or add them to `exempt` with a reason: \
         {missing:?}"
    );
}

/// Every `--flag` the docs show inside a `temper …` command is one temper accepts.
///
/// The sibling of the verb check above, and it was missing. A doc could name a
/// flag temper has never had and nothing would notice — which is not
/// hypothetical: SPEC told the reader to select a machine with
/// `--machine <name>` when the machine name is a **positional** argument, so the
/// one instruction in that paragraph errored with "unexpected argument". It
/// shipped into `temper --llm`, where every agent reading the guide would copy
/// it, and a user found it by running it.
///
/// **Acceptance, not the help listing.** The first version of this compared
/// against `--help` and failed on `--csw`, which works perfectly well — clap
/// takes it as an alias and simply does not print it. The property worth
/// asserting is the one that bites a reader: does the command they were told to
/// run do something other than error. So each flag is put to the parser with the
/// verb it was documented beside. Unlike the verb check, an alias IS good enough
/// here: `--csw` is introduced in the same document that uses it, so it teaches
/// a live spelling rather than keeping a dead one alive.
///
/// Scoped to flags inside a `` `temper …` `` span on purpose. These docs quote
/// other tools constantly — `brew bundle --quiet`, `flatpak install -y
/// --noninteractive`, `rpm -qf`, `install -Dm0644`, `Defaults timestamp_type=tty`
/// — and none of those are temper's to validate. The verb check draws the
/// boundary the same way and for the same reason.
#[test]
fn every_documented_flag_exists() {
    let verbs = canonical_verbs();
    let docs: [(&str, &str); 6] = [
        ("WORKFLOWS.md", include_str!("../../../WORKFLOWS.md")),
        ("README.md", include_str!("../../../README.md")),
        ("ARCHITECTURE.md", include_str!("../../../ARCHITECTURE.md")),
        ("SPEC.md", include_str!("../../../SPEC.md")),
        ("PATTERNS.md", include_str!("../../../PATTERNS.md")),
        ("ROADMAP.md", include_str!("../../../ROADMAP.md")),
    ];

    // (verb, flag) pairs, deduplicated — the same flag is documented many times
    // and each probe is a process spawn.
    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
    for (name, doc) in docs {
        for line in doc.lines() {
            for (i, _) in line.match_indices("temper ") {
                let rest = &line[i + "temper ".len()..];
                // Stop at the closing backtick where there is one, else end of
                // line — a fenced `sh` block has no backticks around it.
                let span = &rest[..rest.find('`').unwrap_or(rest.len())];
                let mut words = span.split_whitespace();
                let Some(first) = words.next() else { continue };
                // A global-only invocation (`temper --llm`) has no verb.
                let verb = verbs.contains(first).then(|| first.to_string());
                for tok in span.split_whitespace() {
                    if !tok.starts_with("--") {
                        continue;
                    }
                    // Trim trailing punctuation and any `=value`.
                    let flag: String = tok
                        .trim_end_matches(|c: char| ",.;:)".contains(c))
                        .split('=')
                        .next()
                        .unwrap_or(tok)
                        .to_string();
                    if flag.len() <= 2 {
                        continue;
                    }
                    let _ = name;
                    pairs.insert((verb.clone().unwrap_or_default(), flag));
                }
            }
        }
    }
    assert!(
        pairs.len() > 8,
        "the scrape found only {} flags in temper commands — it has stopped seeing them: {pairs:?}",
        pairs.len()
    );

    let mut bad: Vec<String> = Vec::new();
    for (verb, flag) in &pairs {
        // `--help` makes clap parse the arguments and then exit, so an accepted
        // flag succeeds and an unknown one errors. Nothing is converged.
        let mut c = Command::cargo_bin("temper").unwrap();
        if !verb.is_empty() {
            c.arg(verb);
        }
        let out = c.arg(flag).arg("--help").output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("unexpected argument") || err.contains("unrecognized") {
            let shown = if verb.is_empty() {
                format!("`temper {flag}`")
            } else {
                format!("`temper {verb} {flag}`")
            };
            bad.push(format!("{shown} — {}", err.lines().next().unwrap_or("").trim()));
        }
    }
    bad.sort();
    bad.dedup();
    assert!(
        bad.is_empty(),
        "these docs tell the reader to run something that errors: {bad:#?}"
    );
}
