//! `SPEC.md` must name every field the parser accepts.
//!
//! AGENTS.md calls SPEC the **parser-of-record**: it "must match the serde
//! structs *exactly*". That claim was prose, and prose cannot fail a build —
//! while `deny_unknown_fields` makes the consequence of a gap sharp in the other
//! direction. A field that exists but is undocumented is unusable by anyone
//! reading `temper --llm`, which is how humans *and* agents learn to author a
//! folder; a field that is documented but does not exist is a parse error the
//! moment someone believes the doc.
//!
//! Source-scraped, like the finding-kind and plan-field checks, and for the same
//! reason: Rust cannot ask a struct for its fields at runtime. The blast radius
//! is one file.

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
    for line in body.lines() {
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

#[test]
fn every_parsed_field_is_documented_in_spec() {
    let src = include_str!("../../temper-core/src/manifest.rs");
    let spec = include_str!("../../../SPEC.md");

    // Every struct the manifest parser exposes to a folder author. A new one
    // belongs here the moment it gains a field somebody has to type.
    let structs = [
        "TemperToml",
        "Machine",
        "Bundle",
        "Ignore",
        "Step",
        "Assert",
        "SetKey",
        "DconfSnapshot",
        "Probe",
        "GnomeExtensionSpec",
        "GitConfig",
    ];

    let mut undocumented: Vec<String> = Vec::new();
    let mut checked = 0;
    for st in structs {
        for f in serde_fields(src, st) {
            checked += 1;
            if !spec.contains(&f) {
                undocumented.push(format!("{st}.{f}"));
            }
        }
    }
    assert!(
        checked > 40,
        "the scrape found only {checked} fields — it has stopped seeing the structs"
    );
    assert!(
        undocumented.is_empty(),
        "these fields parse but appear nowhere in SPEC.md, so nobody reading \
         `temper --llm` can know they exist: {undocumented:?}"
    );
}
