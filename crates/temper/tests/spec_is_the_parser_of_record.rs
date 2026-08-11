//! `SPEC.md` must name every field the parser accepts.
//!
//! AGENTS.md calls SPEC the **parser-of-record**: it "must match the serde
//! structs *exactly*", and promises "a test scrapes the structs and fails on a
//! field SPEC does not name, so this half is mechanical". Two things had to
//! change before that was true.
//!
//! The struct list was hand-written and named 11 of the 19 structs that derive
//! `Deserialize` — so `[brew].trust`, `[update].mode`, `[ui].icons`
//! and every nested assert shape (`contains_line`, `mode`,
//! `group`, `json_semantic`) were outside the guarantee. The list is derived
//! from the source now, so a new struct joins it by existing.
//!
//! And the match was `spec.contains(field)` — a bare substring over 500 lines of
//! prose. `os` occurs 17 times in SPEC as an English word, `key` 26, `in` 39, so
//! every short field name was satisfied for free, including `Step.in_file`,
//! which is written `in` — the flagship rename case was the least checkable one.
//! A field now has to appear the way a folder author would write it: as an
//! assignment, or as a table header.
//!
//! Source-scraped, like the finding-kind and plan-field checks, and for the same
//! reason: Rust cannot ask a struct for its fields at runtime.

/// Every struct in the manifest that a folder author can write into — i.e.
/// everything deriving `Deserialize`.
fn deserializable_structs(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let l = line.trim();
        if !l.starts_with("pub struct ") {
            continue;
        }
        let name = l["pub struct ".len()..]
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches('{')
            .trim()
            .to_string();
        if name.is_empty() {
            continue;
        }
        // Look back over the attributes attached to this struct for a
        // `Deserialize` derive. Four lines covers `#[derive(...)]` plus the
        // `#[serde(...)]` attributes that sit between it and the struct.
        let preceding: Vec<&str> = src.lines().take(i).collect();
        let derived = preceding
            .iter()
            .rev()
            .take(4)
            .any(|a| a.contains("derive") && a.contains("Deserialize"));
        if derived {
            out.push(name);
        }
    }
    out
}

/// Field names a struct declares, honouring `#[serde(rename = "…")]` — the
/// manifest spells a few fields differently from the Rust identifier (`in_file`
/// is written `in`), and it is the *written* name SPEC has to carry.
fn serde_fields(src: &str, struct_name: &str) -> Vec<String> {
    let start = match src.find(&format!("pub struct {struct_name} {{")) {
        Some(i) => i,
        None => panic!("struct `{struct_name}` not found — was it renamed?"),
    };
    let body = &src[start..][..src[start..].find("\n}").expect("struct end")];
    let mut out = Vec::new();
    let mut renamed: Option<String> = None;
    // `.skip(1)`: the `pub struct X {` line is not a field.
    for line in body.lines().skip(1) {
        let l = line.trim();
        if l.starts_with("#[serde") {
            if let Some(i) = l.find("rename = \"") {
                let rest = &l[i + "rename = \"".len()..];
                if let Some(end) = rest.find('"') {
                    renamed = Some(rest[..end].to_string());
                }
            }
            continue;
        }
        if let Some(rest) = l.strip_prefix("pub ") {
            if let Some((name, _)) = rest.split_once(':') {
                out.push(renamed.take().unwrap_or_else(|| name.trim().to_string()));
            }
        }
    }
    out
}

/// Whether SPEC shows this field the way a folder author would write it.
///
/// Either an assignment — `name =` in an example, `name:` in an example's
/// comment, or inline in prose as `` `when = { binary = "…" }` `` — or a table
/// header, for the fields whose value is a table (`[ignore]`, `[[machine]]`,
/// `[[machine.dconf]]`). Prose *mentioning* the word does not count, which is
/// the whole point: that is what let `os`, `key` and `in` pass without being
/// documented at all.
fn documented(spec: &str, field: &str) -> bool {
    let assigned = spec.lines().any(|l| {
        let mut rest = l;
        while let Some(i) = rest.find(field) {
            let before = rest[..i].chars().next_back();
            let after = rest[i + field.len()..].trim_start();
            let boundary_ok = before.is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '.');
            if boundary_ok && (after.starts_with('=') || after.starts_with(':')) {
                return true;
            }
            rest = &rest[i + field.len()..];
        }
        false
    });
    if assigned {
        return true;
    }
    // `[field]`, `[[field]]`, `[parent.field]`, `[[parent.field]]` — reading only
    // as far as the closing bracket, because a SPEC example almost always has a
    // trailing `# comment` on the same line.
    spec.lines().any(|l| {
        let t = l.trim();
        if !t.starts_with('[') {
            return false;
        }
        let inner = t.trim_start_matches('[');
        let Some(end) = inner.find(']') else {
            return false;
        };
        let name = &inner[..end];
        name == field || name.rsplit('.').next() == Some(field)
    })
}

#[test]
fn every_parsed_field_is_documented_in_spec() {
    let src = include_str!("../../temper-core/src/manifest.rs");
    let spec = include_str!("../../../SPEC.md");

    let structs = deserializable_structs(src);
    // A floor, not a count: it exists to catch the scrape silently matching
    // nothing, which would make the whole test vacuous. Lower it only when a
    // struct is deliberately deleted — never to make a red run green, because
    // "the scrape stopped seeing them" and "there are fewer" look identical here
    // and only one of them is fine.
    assert!(
        structs.len() >= 18,
        "the scrape found only {} deserializable structs — it has stopped seeing \
         them: {structs:?}",
        structs.len()
    );

    let mut undocumented: Vec<String> = Vec::new();
    let mut checked = 0;
    for st in &structs {
        for f in serde_fields(src, st) {
            checked += 1;
            if !documented(spec, &f) {
                undocumented.push(format!("{st}.{f}"));
            }
        }
    }
    assert!(
        checked > 100,
        "the scrape found only {checked} fields — it has stopped seeing the structs"
    );
    assert!(
        undocumented.is_empty(),
        "these fields parse but SPEC.md never shows them being written, so nobody \
         reading `temper --llm` can know they exist: {undocumented:?}"
    );
}

/// The guarantee is only worth what its matcher is worth.
///
/// The previous matcher was `spec.contains(field)`, which a field named `os` or
/// `in` satisfied from English prose. If this test ever passes, the check above
/// has gone back to accepting mentions instead of usages.
#[test]
fn the_matcher_does_not_accept_a_mere_mention() {
    let spec = include_str!("../../../SPEC.md");

    // Words that certainly appear in SPEC as prose, and are not written as
    // fields anywhere. If `documented` says yes to these, it is matching prose.
    for word in ["machine", "folder", "spec", "temper"] {
        let mentioned = spec.contains(word);
        assert!(mentioned, "`{word}` should appear in SPEC as prose");
    }
    for invented in ["frobnicate", "totally_made_up_field", "zzz_not_a_field"] {
        assert!(
            !documented(spec, invented),
            "`{invented}` is not a field and SPEC never writes it"
        );
    }
}
