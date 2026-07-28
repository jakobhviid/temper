//! Comment/format-preserving JSONC edits for the `setkey(json)` backend.
//!
//! Real-world JSON targets carry comments and trailing commas (JSONC) —
//! `opencode.jsonc`, VS Code `settings.json`, `tsconfig.json`, anything a
//! comment-preserving tool co-writes. A strict serde round-trip would reject
//! them on read and strip every comment on write. So we parse **tolerantly**
//! (jsonc-parser): to a serde value for drift/idempotency checks, and to an AST
//! to locate byte ranges so a `set` is a minimal splice on the original text —
//! every untouched comment, sibling key, and bit of formatting survives.
//!
//! jsonc-parser reports ranges as **byte** offsets, so all slicing is UTF-8-safe
//! (e.g. Danish comments in a config).

use anyhow::{anyhow, bail, Result};
use jsonc_parser::ast::{Array, Object};
use jsonc_parser::common::Ranged;
use jsonc_parser::{parse_to_ast, CollectOptions, ParseOptions};
use serde_json::Value as Json;

/// Tolerant read: parse JSONC (comments, trailing commas) to a serde value.
/// Empty/blank text is an empty object (so an absent key reads as "not set").
pub fn parse_value(text: &str) -> Result<Json> {
    if text.trim().is_empty() {
        return Ok(Json::Object(Default::default()));
    }
    jsonc_parser::parse_to_serde_value(text, &ParseOptions::default())
        .map_err(|e| anyhow!("parsing JSONC: {e}"))?
        .ok_or_else(|| anyhow!("JSONC contained no value"))
}

/// Set `path` (dotted key, pre-split) to `value` in JSONC `text`, preserving
/// comments/formatting/siblings via a minimal splice. Creates intermediate
/// objects for a deep path (`mcp.searxng.type` when `mcp` is absent). With
/// `append`, unions `value` into an array-valued leaf (creating `[value]` if
/// absent). `text` may be empty ("" → a fresh `{}`). Returns the new full text.
///
/// The caller is expected to have already checked idempotency (via `parse_value`
/// + a value compare) and only call this when a change is actually needed.
pub fn set(text: &str, path: &[&str], value: &Json, append: bool) -> Result<String> {
    if path.is_empty() {
        bail!("setkey json: empty key");
    }
    // A blank file starts life as an empty object.
    let seed = String::from("{}\n");
    let src: &str = if text.trim().is_empty() { &seed } else { text };

    let parsed = parse_to_ast(src, &CollectOptions::default(), &ParseOptions::default())
        .map_err(|e| anyhow!("target is not valid JSONC: {e}"))?;
    let root = parsed
        .value
        .as_ref()
        .ok_or_else(|| anyhow!("target contained no JSON value"))?;
    let mut obj = root
        .as_object()
        .ok_or_else(|| anyhow!("setkey json: the top level is not an object"))?;

    // Descend through the intermediate segments as far as they already exist.
    let mut depth = 0;
    while depth + 1 < path.len() {
        match obj.get(path[depth]) {
            Some(prop) => match prop.value.as_object() {
                Some(child) => {
                    obj = child;
                    depth += 1;
                }
                None => bail!("setkey json: intermediate key `{}` is not an object", path[depth]),
            },
            None => break, // the rest of the path must be created under `obj`
        }
    }

    let remaining = &path[depth..];
    if remaining.len() == 1 {
        let key = remaining[0];
        match obj.get(key) {
            // Key present: append into its array, or replace its value span.
            Some(prop) if append => {
                let arr = prop
                    .value
                    .as_array()
                    .ok_or_else(|| anyhow!("setkey append: `{key}` is not an array"))?;
                insert_into_array(src, arr, value)
            }
            Some(prop) => {
                let r = prop.value.range();
                Ok(replace(src, r.start, r.end, &serialize(value)))
            }
            // Key absent: insert a fresh property into this object.
            None => insert_property(src, obj, &leaf_item(key, value, append)),
        }
    } else {
        // An intermediate segment is missing → insert the whole nested chain.
        insert_property(src, obj, &nested_item(remaining, value, append))
    }
}

/// `"key": <value>` (or `"key": [<value>]` when appending to a new array).
fn leaf_item(key: &str, value: &Json, append: bool) -> String {
    let v = if append {
        format!("[{}]", serialize(value))
    } else {
        serialize(value)
    };
    format!("{}: {v}", serialize(&Json::from(key)))
}

/// `"seg": { "next": { … "leaf": <value> } }` — the nested objects a deep path
/// needs when its parents don't exist yet.
fn nested_item(path: &[&str], value: &Json, append: bool) -> String {
    if path.len() == 1 {
        return leaf_item(path[0], value, append);
    }
    format!(
        "{}: {{ {} }}",
        serialize(&Json::from(path[0])),
        nested_item(&path[1..], value, append)
    )
}

/// Compact JSON for an inserted/replaced value (existing formatting elsewhere is
/// untouched; only the changed value is written, so compact is fine).
fn serialize(value: &Json) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into())
}

// ---- splice helpers (byte-offset text edits) ---------------------------------

fn splice(src: &str, at: usize, ins: &str) -> String {
    let mut s = String::with_capacity(src.len() + ins.len());
    s.push_str(&src[..at]);
    s.push_str(ins);
    s.push_str(&src[at..]);
    s
}

/// Replace `src[a..b]` with `ins`.
fn replace(src: &str, a: usize, b: usize, ins: &str) -> String {
    let mut s = String::with_capacity(src.len() - (b - a) + ins.len());
    s.push_str(&src[..a]);
    s.push_str(ins);
    s.push_str(&src[b..]);
    s
}

/// Leading whitespace of the line `pos` sits on (its indentation).
fn line_indent(src: &str, pos: usize) -> String {
    let line_start = src[..pos].rfind('\n').map(|n| n + 1).unwrap_or(0);
    src[line_start..pos]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Append `item` after the last element of a sequence, matching its formatting
/// (multiline indentation, and any existing trailing comma). `last_start`/
/// `last_end` bracket the last item; `close` is the closing `]`/`}`.
fn append_item(src: &str, last_start: usize, last_end: usize, close: usize, multiline: bool, item: &str) -> String {
    let indent = line_indent(src, last_start);
    let tail = &src[last_end..close];
    if let Some(rel) = tail.find(',') {
        // A trailing comma already separates us from the previous item.
        let at = last_end + rel + 1;
        let sep = if multiline { format!("\n{indent}") } else { " ".to_string() };
        splice(src, at, &format!("{sep}{item}"))
    } else {
        let sep = if multiline { format!(",\n{indent}") } else { ", ".to_string() };
        splice(src, last_end, &format!("{sep}{item}"))
    }
}

fn insert_property(src: &str, obj: &Object<'_>, item: &str) -> Result<String> {
    let close = src[..obj.range.end]
        .rfind('}')
        .ok_or_else(|| anyhow!("could not locate the end of the JSON object"))?;
    let multiline = src[obj.range.start..close].contains('\n');
    Ok(match obj.properties.last() {
        Some(last) => append_item(src, last.range.start, last.range.end, close, multiline, item),
        None if multiline => splice(src, obj.range.start + 1, &format!("\n  {item}\n")),
        None => splice(src, obj.range.start + 1, &format!(" {item} ")),
    })
}

fn insert_into_array(src: &str, arr: &Array<'_>, value: &Json) -> Result<String> {
    let close = src[..arr.range.end]
        .rfind(']')
        .ok_or_else(|| anyhow!("could not locate the end of the JSON array"))?;
    let multiline = src[arr.range.start..close].contains('\n');
    let item = serialize(value);
    Ok(match arr.elements.last() {
        Some(last) => append_item(src, last.range().start, last.range().end, close, multiline, &item),
        None => splice(src, arr.range.start + 1, &item),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Re-read the value at a dotted path (tolerant), for asserting outcomes.
    fn at(text: &str, path: &[&str]) -> Json {
        let mut cur = parse_value(text).unwrap();
        for p in path {
            cur = cur.as_object().unwrap().get(*p).cloned().unwrap();
        }
        cur
    }

    #[test]
    fn read_tolerates_comments_and_trailing_commas() {
        let src = "{\n  // a comment\n  \"share\": \"disabled\",\n}\n";
        let v = parse_value(src).unwrap();
        assert_eq!(v["share"], Json::from("disabled"));
    }

    #[test]
    fn replace_existing_leaf_preserves_comments_and_siblings() {
        let src = "{\n  \"$schema\": \"x\",\n  // keep me\n  \"share\": \"auto\"\n}\n";
        let out = set(src, &["share"], &Json::from("disabled"), false).unwrap();
        assert!(out.contains("// keep me"), "comment lost:\n{out}");
        assert!(out.contains("\"$schema\": \"x\""), "sibling lost:\n{out}");
        assert_eq!(at(&out, &["share"]), Json::from("disabled"));
    }

    #[test]
    fn create_deep_path_when_parents_absent() {
        let src = "{\n  // header\n  \"share\": \"disabled\"\n}\n";
        let out = set(src, &["mcp", "searxng", "type"], &Json::from("local"), false).unwrap();
        assert!(out.contains("// header"), "comment lost:\n{out}");
        assert_eq!(at(&out, &["mcp", "searxng", "type"]), Json::from("local"));
        assert_eq!(at(&out, &["share"]), Json::from("disabled"));
    }

    #[test]
    fn insert_new_key_into_existing_object() {
        let src = "{\n  \"a\": 1\n}\n";
        let out = set(src, &["b"], &Json::from(2), false).unwrap();
        assert_eq!(at(&out, &["a"]), Json::from(1));
        assert_eq!(at(&out, &["b"]), Json::from(2));
    }

    #[test]
    fn empty_file_becomes_an_object() {
        let out = set("", &["share"], &Json::from("disabled"), false).unwrap();
        assert_eq!(at(&out, &["share"]), Json::from("disabled"));
    }

    #[test]
    fn append_unions_into_array_and_creates_it() {
        // create the array
        let out = set("{\n  \"x\": 1\n}\n", &["plugins"], &Json::from("p"), true).unwrap();
        assert_eq!(at(&out, &["plugins"]), Json::Array(vec![Json::from("p")]));
        // append to an existing array, keeping the first element + a comment
        let src = "{\n  \"plugins\": [\n    // first\n    \"a\"\n  ]\n}\n";
        let out2 = set(src, &["plugins"], &Json::from("b"), true).unwrap();
        assert!(out2.contains("// first"), "comment lost:\n{out2}");
        assert_eq!(
            at(&out2, &["plugins"]),
            Json::Array(vec![Json::from("a"), Json::from("b")])
        );
    }

    #[test]
    fn object_value_is_supported() {
        let val: Json = serde_json::json!({ "type": "local", "url": "http://x" });
        let out = set("{}\n", &["mcp", "searxng"], &val, false).unwrap();
        assert_eq!(at(&out, &["mcp", "searxng", "type"]), Json::from("local"));
        assert_eq!(at(&out, &["mcp", "searxng", "url"]), Json::from("http://x"));
    }

    #[test]
    fn descending_into_a_scalar_errors() {
        // `a` is a string, so `a.b` can't be set without clobbering it.
        assert!(set("{ \"a\": \"s\" }", &["a", "b"], &Json::from(1), false).is_err());
    }

    #[test]
    fn utf8_comment_survives() {
        let src = "{\n  // sæt standardmodellen — æøå\n  \"share\": \"auto\"\n}\n";
        let out = set(src, &["share"], &Json::from("disabled"), false).unwrap();
        assert!(out.contains("æøå"), "utf8 comment lost:\n{out}");
    }
}
