// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The published JSON Schemas under `schema/`, read by a validator that knows
//! the keywords they use and fails on any other, so that a keyword added to a
//! schema is never passed over here as satisfied.

#![allow(
    dead_code,
    reason = "each test binary that includes this module uses what it needs of it"
)]

use std::path::Path;

use serde_json::{Map, Value};

/// What `value` breaks of the schema published as `schema/<file>`, one line
/// a refusal; empty where it holds.
pub fn refusals(file: &str, value: &Value) -> Vec<String> {
    let schema = published(file);
    let mut refused = Vec::new();
    check(&schema, &schema, value, "$", &mut refused);
    refused
}

fn published(file: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema")
        .join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} is published in the tree: {error}", path.display()));
    serde_json::from_str(&text).expect("a published schema is JSON")
}

/// Checks `value` against `schema` for the keywords the published schema
/// uses, and fails on any other, so that a keyword added to the schema is not
/// passed over here as satisfied.
fn check(root: &Value, schema: &Value, value: &Value, at: &str, refused: &mut Vec<String>) {
    let Value::Object(keywords) = schema else {
        panic!("{at}: a schema is an object");
    };
    for (keyword, expected) in keywords {
        match keyword.as_str() {
            "$schema" | "$id" | "$defs" | "title" | "description" => {}
            "$ref" => {
                let (file, name) = expected
                    .as_str()
                    .and_then(|r| r.split_once("#/$defs/"))
                    .expect("a reference into the $defs of this schema or another");
                if file.is_empty() {
                    check(root, &root["$defs"][name], value, at, refused);
                } else {
                    let other = published(file);
                    check(&other, &other["$defs"][name], value, at, refused);
                }
            }
            "type" => {
                let types: Vec<&str> = match expected {
                    Value::String(one) => vec![one.as_str()],
                    Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
                    _ => panic!("{at}: a type is a name or names"),
                };
                if !types.iter().any(|name| is_type(value, name)) {
                    refused.push(format!("{at}: {value} is not {types:?}"));
                }
            }
            "const" if value != expected => refused.push(format!("{at}: {value} ≠ {expected}")),
            "enum" if !expected.as_array().is_some_and(|all| all.contains(value)) => {
                refused.push(format!("{at}: {value} is not one of {expected}"));
            }
            "const" | "enum" => {}
            "minimum" => {
                let least = expected.as_f64().expect("a minimum is a number");
                if value.as_f64().is_some_and(|n| n < least) {
                    refused.push(format!("{at}: {value} < {least}"));
                }
            }
            "pattern" => {
                assert_eq!(
                    expected, "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$",
                    "{at}: the one pattern this check reads"
                );
                if value.as_str().is_some_and(|text| !is_utc_second(text)) {
                    refused.push(format!("{at}: {value} is not RFC 3339 UTC to the second"));
                }
            }
            "required" => {
                for name in expected.as_array().expect("required is a list") {
                    let name = name.as_str().expect("a property name");
                    if value.as_object().is_some_and(|o| !o.contains_key(name)) {
                        refused.push(format!("{at}: no {name}"));
                    }
                }
            }
            "properties" => {
                for (name, property) in expected.as_object().expect("properties is an object") {
                    if let Some(field) = value.get(name) {
                        check(root, property, field, &format!("{at}.{name}"), refused);
                    }
                }
            }
            "additionalProperties" => {
                let Some(fields) = value.as_object() else {
                    continue;
                };
                let named = keywords.get("properties").and_then(Value::as_object);
                for (name, field) in extra(fields, named) {
                    match expected {
                        Value::Bool(false) => refused.push(format!("{at}: {name} is not named")),
                        Value::Object(_) => {
                            check(root, expected, field, &format!("{at}.{name}"), refused);
                        }
                        _ => panic!("{at}: additionalProperties is false or a schema"),
                    }
                }
            }
            "items" => {
                for (index, item) in value.as_array().into_iter().flatten().enumerate() {
                    check(root, expected, item, &format!("{at}[{index}]"), refused);
                }
            }
            "oneOf" => {
                let matching = expected
                    .as_array()
                    .expect("oneOf is a list")
                    .iter()
                    .filter(|one| {
                        let mut inner = Vec::new();
                        check(root, one, value, at, &mut inner);
                        inner.is_empty()
                    })
                    .count();
                if matching != 1 {
                    refused.push(format!("{at}: {matching} of oneOf match {value}"));
                }
            }
            other => panic!("{at}: the schema uses `{other}`, which this check does not read"),
        }
    }
}

fn extra<'a>(
    fields: &'a Map<String, Value>,
    named: Option<&'a Map<String, Value>>,
) -> impl Iterator<Item = (&'a String, &'a Value)> {
    fields
        .iter()
        .filter(move |(name, _)| named.is_none_or(|named| !named.contains_key(*name)))
}

fn is_type(value: &Value, name: &str) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        other => panic!("no JSON type `{other}`"),
    }
}

fn is_utc_second(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 20
        && bytes.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            10 => *b == b'T',
            13 | 16 => *b == b':',
            19 => *b == b'Z',
            _ => b.is_ascii_digit(),
        })
}
