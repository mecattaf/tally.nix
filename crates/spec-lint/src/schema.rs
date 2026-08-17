//! The byte oracle behind `[L14]`'s schema half: `trace.json` read against
//! `specs/zeta/contracts/trace.schema.json`.
//!
//! This is a JSON Schema 2020-12 evaluator for exactly the keyword set that
//! contract uses, and it refuses to run over any other. A validator that
//! silently ignores a keyword it does not implement is the `--list-only` flake
//! attribute again — it reports success over bytes it never read — so an
//! unknown keyword is a hard error naming itself, not a pass.
//!
//! The contract file is the authority; this module never carries a copy of it.

use std::collections::BTreeSet;

use serde_json::Value;

/// Every keyword this evaluator implements. `$schema`, `$id`, `title`,
/// `description`, and `format` are annotations: read, asserted on nothing.
const KEYWORDS: [&str; 20] = [
    "$schema",
    "$id",
    "$defs",
    "$ref",
    "title",
    "description",
    "format",
    "type",
    "const",
    "enum",
    "pattern",
    "minLength",
    "minimum",
    "minItems",
    "required",
    "properties",
    "additionalProperties",
    "unevaluatedProperties",
    "items",
    "oneOf",
];

/// One instance location that failed, with the reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Failure {
    /// A JSON pointer into the instance, `` for the root.
    pub at: String,
    pub message: String,
}

impl Failure {
    fn new(at: &str, message: impl Into<String>) -> Self {
        Self {
            at: if at.is_empty() {
                "/".to_owned()
            } else {
                at.to_owned()
            },
            message: message.into(),
        }
    }
}

/// Validate `instance` against `schema`. `Err` means the schema reaches past
/// this evaluator — never that the instance is invalid.
pub fn validate(schema: &Value, instance: &Value) -> anyhow::Result<Vec<Failure>> {
    audit(schema)?;
    let mut failures = Vec::new();
    Evaluator { root: schema }.eval(schema, instance, "", &mut failures);
    Ok(failures)
}

/// Walk the whole schema up front and refuse anything unimplemented, so the
/// verdict is never narrower than the contract.
fn audit(schema: &Value) -> anyhow::Result<()> {
    match schema {
        Value::Bool(_) => Ok(()),
        Value::Object(members) => {
            for (keyword, value) in members {
                if !KEYWORDS.contains(&keyword.as_str()) {
                    anyhow::bail!(
                        "the trace schema uses `{keyword}`, which this evaluator does not implement"
                    );
                }
                match keyword.as_str() {
                    "properties" | "$defs" => {
                        for member in value.as_object().into_iter().flatten() {
                            audit(member.1)?;
                        }
                    }
                    "items" | "additionalProperties" | "unevaluatedProperties" => audit(value)?,
                    "oneOf" => {
                        for branch in value.as_array().into_iter().flatten() {
                            audit(branch)?;
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        _ => anyhow::bail!("a schema is an object or a boolean"),
    }
}

struct Evaluator<'a> {
    root: &'a Value,
}

impl Evaluator<'_> {
    /// Evaluate one subschema, returning the instance property names it
    /// evaluated — what `unevaluatedProperties` is measured against.
    fn eval(
        &self,
        schema: &Value,
        instance: &Value,
        at: &str,
        failures: &mut Vec<Failure>,
    ) -> BTreeSet<String> {
        let mut evaluated: BTreeSet<String> = BTreeSet::new();
        match schema {
            Value::Bool(true) => return evaluated,
            Value::Bool(false) => {
                failures.push(Failure::new(at, "no value is admitted here"));
                return evaluated;
            }
            _ => {}
        }
        let Some(members) = schema.as_object() else {
            return evaluated;
        };

        if let Some(reference) = members.get("$ref").and_then(Value::as_str) {
            match self.resolve(reference) {
                Some(target) => evaluated.extend(self.eval(target, instance, at, failures)),
                None => failures.push(Failure::new(
                    at,
                    format!("the schema reference `{reference}` does not resolve"),
                )),
            }
        }

        if let Some(expected) = members.get("type").and_then(Value::as_str) {
            if !type_matches(expected, instance) {
                failures.push(Failure::new(
                    at,
                    format!("expected type `{expected}`, found `{}`", kind(instance)),
                ));
                return evaluated;
            }
        }

        if let Some(expected) = members.get("const") {
            if instance != expected {
                failures.push(Failure::new(
                    at,
                    format!("expected the constant {expected}"),
                ));
            }
        }
        if let Some(admitted) = members.get("enum").and_then(Value::as_array) {
            if !admitted.contains(instance) {
                failures.push(Failure::new(
                    at,
                    format!(
                        "expected one of {}",
                        admitted
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<String>>()
                            .join(", ")
                    ),
                ));
            }
        }

        if let Some(text) = instance.as_str() {
            if let Some(pattern) = members.get("pattern").and_then(Value::as_str) {
                match regex::Regex::new(pattern) {
                    Ok(compiled) if !compiled.is_match(text) => failures.push(Failure::new(
                        at,
                        format!("`{text}` does not match `{pattern}`"),
                    )),
                    Ok(_) => {}
                    Err(_) => failures.push(Failure::new(
                        at,
                        format!("the schema pattern `{pattern}` does not compile"),
                    )),
                }
            }
            if let Some(minimum) = members.get("minLength").and_then(Value::as_u64) {
                if (text.chars().count() as u64) < minimum {
                    failures.push(Failure::new(
                        at,
                        format!("shorter than {minimum} character(s)"),
                    ));
                }
            }
        }

        if let Some(minimum) = members.get("minimum").and_then(Value::as_i64) {
            if instance.as_i64().is_some_and(|number| number < minimum) {
                failures.push(Failure::new(at, format!("below the minimum {minimum}")));
            }
        }

        if let Some(items) = instance.as_array() {
            if let Some(minimum) = members.get("minItems").and_then(Value::as_u64) {
                if (items.len() as u64) < minimum {
                    failures.push(Failure::new(at, format!("fewer than {minimum} item(s)")));
                }
            }
            if let Some(subschema) = members.get("items") {
                for (index, item) in items.iter().enumerate() {
                    self.eval(subschema, item, &format!("{at}/{index}"), failures);
                }
            }
        }

        if let Some(object) = instance.as_object() {
            for name in members
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !object.contains_key(name) {
                    failures.push(Failure::new(at, format!("the member `{name}` is required")));
                }
            }

            let declared = members.get("properties").and_then(Value::as_object);
            for (name, value) in object {
                let Some(subschema) = declared.and_then(|declared| declared.get(name)) else {
                    continue;
                };
                evaluated.insert(name.clone());
                self.eval(subschema, value, &format!("{at}/{name}"), failures);
            }

            if let Some(additional) = members.get("additionalProperties") {
                for (name, value) in object {
                    if declared.is_some_and(|declared| declared.contains_key(name)) {
                        continue;
                    }
                    evaluated.insert(name.clone());
                    if additional == &Value::Bool(false) {
                        failures.push(Failure::new(
                            at,
                            format!("the member `{name}` is not admitted here"),
                        ));
                    } else {
                        self.eval(additional, value, &format!("{at}/{name}"), failures);
                    }
                }
            }
        }

        if let Some(branches) = members.get("oneOf").and_then(Value::as_array) {
            let mut passing: Vec<BTreeSet<String>> = Vec::new();
            for branch in branches {
                let mut scratch = Vec::new();
                let seen = self.eval(branch, instance, at, &mut scratch);
                if scratch.is_empty() {
                    passing.push(seen);
                }
            }
            match passing.len() {
                1 => evaluated.extend(passing.remove(0)),
                count => failures.push(Failure::new(
                    at,
                    format!(
                        "matches {count} of the {} admitted shapes, not one",
                        branches.len()
                    ),
                )),
            }
        }

        if members.get("unevaluatedProperties") == Some(&Value::Bool(false)) {
            for name in instance
                .as_object()
                .into_iter()
                .flatten()
                .map(|(name, _)| name)
            {
                if !evaluated.contains(name) {
                    failures.push(Failure::new(
                        at,
                        format!("the member `{name}` is not admitted by any matching shape"),
                    ));
                }
            }
        }

        evaluated
    }

    /// Only the two reference forms the contract uses: the root and `$defs`.
    fn resolve<'b>(&'b self, reference: &str) -> Option<&'b Value> {
        match reference {
            "#" => Some(self.root),
            _ => self
                .root
                .get("$defs")?
                .get(reference.strip_prefix("#/$defs/")?),
        }
    }
}

fn type_matches(expected: &str, instance: &Value) -> bool {
    match expected {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "integer" => instance.as_i64().is_some() || instance.as_u64().is_some(),
        "number" => instance.is_number(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        _ => false,
    }
}

fn kind(instance: &Value) -> &'static str {
    match instance {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::validate;

    fn schema() -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["rows"],
            "properties": { "rows": { "type": "array", "items": { "$ref": "#/$defs/row" } } },
            "$defs": {
                "row": {
                    "type": "object",
                    "required": ["kind", "claim"],
                    "properties": {
                        "kind": { "enum": ["sitting", "release"] },
                        "claim": { "type": "string", "pattern": "^[0-9]+\\.[0-9]+$" }
                    },
                    "oneOf": [
                        { "properties": { "kind": { "const": "sitting" },
                                          "acceptance": { "type": "array", "minItems": 1 } },
                          "required": ["acceptance"] },
                        { "properties": { "kind": { "const": "release" },
                                          "witness": { "type": "string", "minLength": 1 } },
                          "required": ["witness"] }
                    ],
                    "unevaluatedProperties": false
                }
            }
        })
    }

    fn failures(instance: &serde_json::Value) -> Vec<String> {
        validate(&schema(), instance)
            .expect("the schema is inside the implemented keyword set")
            .into_iter()
            .map(|failure| format!("{}: {}", failure.at, failure.message))
            .collect()
    }

    #[test]
    fn a_conforming_instance_reports_nothing() {
        let instance = json!({ "rows": [
            { "kind": "sitting", "claim": "1.1", "acceptance": ["green"] },
            { "kind": "release", "claim": "1.1", "witness": "summary/complete" }
        ] });
        assert_eq!(failures(&instance), Vec::<String>::new());
    }

    #[test]
    fn each_keyword_the_contract_uses_bites() {
        assert!(failures(&json!({ "rows": [] , "extra": 1 }))
            .iter()
            .any(|failure| failure.contains("`extra` is not admitted")));
        assert!(failures(&json!({}))
            .iter()
            .any(|failure| failure.contains("`rows` is required")));
        assert!(failures(
            &json!({ "rows": [{ "kind": "sitting", "claim": "one", "acceptance": ["g"] }] })
        )
        .iter()
        .any(|failure| failure.contains("does not match")));
        assert!(
            failures(&json!({ "rows": [{ "kind": "audit", "claim": "1.1" }] }))
                .iter()
                .any(|failure| failure.contains("expected one of"))
        );
        assert!(
            failures(&json!({ "rows": [{ "kind": "sitting", "claim": "1.1" }] }))
                .iter()
                .any(|failure| failure.contains("not one"))
        );
        assert!(
            failures(&json!({ "rows": [{ "kind": "sitting", "claim": "1.1",
                                             "acceptance": [], "witness": "w" }] }))
            .iter()
            .any(|failure| failure.contains("not one"))
        );
        assert!(
            failures(&json!({ "rows": [{ "kind": "sitting", "claim": "1.1",
                                             "acceptance": ["g"], "stray": 1 }] }))
            .iter()
            .any(|failure| failure.contains("`stray` is not admitted by any matching shape"))
        );
        assert!(failures(&json!({ "rows": 7 }))
            .iter()
            .any(|failure| failure.contains("expected type `array`")));
    }

    #[test]
    fn a_keyword_the_evaluator_does_not_implement_is_an_error_not_a_pass() {
        let reaching =
            json!({ "type": "object", "patternProperties": { "^a": { "type": "string" } } });
        let error = validate(&reaching, &json!({})).expect_err("the keyword is refused");
        assert!(error.to_string().contains("patternProperties"));
    }
}
