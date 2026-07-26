use std::collections::BTreeSet;
use std::ops::ControlFlow;
use std::path::Path;

use boa_ast::declaration::{Binding, Declaration, ExportDeclaration, LexicalDeclaration};
use boa_ast::expression::access::{PropertyAccess, PropertyAccessField};
use boa_ast::expression::literal::{LiteralKind, ObjectLiteral, PropertyDefinition};
use boa_ast::expression::operator::unary::UnaryOp;
use boa_ast::expression::{Call, Expression, Identifier};
use boa_ast::scope::Scope;
use boa_ast::visitor::{VisitWith, Visitor};
use boa_ast::{Module, ModuleItem, Span, Spanned};
use boa_interner::Interner;
use boa_parser::error::Error as ParseError;
use boa_parser::lexer::Error as LexError;
use boa_parser::Parser;
use boa_parser::Source;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::catalog::{validate_catalog_semantics, validate_catalog_value};
use crate::{
    resolve_members, Catalog, FlowError, SelectorOptions, SourceLocation, DEFAULT_ITERATION_CAP,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Meta {
    pub name: String,
    pub description: String,
    pub pools: Vec<String>,
    pub args_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_cap: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selectors: Vec<String>,
}

impl Meta {
    #[must_use]
    pub fn iteration_cap(&self) -> u32 {
        self.iteration_cap.unwrap_or(DEFAULT_ITERATION_CAP)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CheckOptions<'a> {
    pub args: Option<&'a Value>,
    pub catalog: Option<&'a Catalog>,
    pub catalog_hash: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct CheckedFlow {
    pub meta: Meta,
    pub meta_json: Value,
    pub script_source: String,
    pub(crate) host_call_sites: Vec<SourceLocation>,
}

pub fn check_script(
    source: &str,
    path: Option<&Path>,
    options: CheckOptions<'_>,
) -> Result<CheckedFlow, FlowError> {
    let mut interner = Interner::default();
    let mut parser = Parser::new(Source::from_reader(source.as_bytes(), path));
    let module = parser
        .parse_module(&Scope::new_global(), &mut interner)
        .map_err(parse_error)?;
    let (meta_json, export_location) = extract_meta(&module, &interner)?;
    let meta: Meta = serde_json::from_value(meta_json.clone()).map_err(|error| {
        FlowError::new(
            "FlowMetaError",
            "meta-invalid",
            format!("meta does not match the flow dialect: {error}"),
        )
        .at(export_location)
    })?;
    validate_meta(&meta, export_location)?;

    let mut lint = DeterminismLint {
        interner: &interner,
        meta: &meta,
        host_call_sites: Vec::new(),
    };
    if let ControlFlow::Break(error) = module.visit_with(&mut lint) {
        return Err(error);
    }
    let host_call_sites = lint.host_call_sites;

    if let Some(args) = options.args {
        validate_instance(
            &meta.args_schema,
            args,
            "FlowArgsError",
            "args-schema-mismatch",
            "args do not match meta.argsSchema",
            export_location,
        )?;
    }
    if let Some(catalog) = options.catalog {
        let value = serde_json::to_value(catalog).map_err(|error| {
            FlowError::new(
                "FlowCatalogError",
                "catalog-schema-mismatch",
                format!("cannot serialize catalog for validation: {error}"),
            )
            .at(export_location)
        })?;
        validate_catalog_value(&value).map_err(|error| error.at_if_missing(export_location))?;
        validate_catalog_semantics(catalog)
            .map_err(|error| error.at_if_missing(export_location))?;
        let hash = options.catalog_hash.unwrap_or("sha256:catalog-check");
        for selector in &meta.selectors {
            resolve_members(catalog, hash, selector, &SelectorOptions::default())
                .map_err(|error| error.at_if_missing(export_location))?;
        }
    }

    let script_source = strip_meta_export(source)?;
    let mut script_interner = Interner::default();
    Parser::new(Source::from_reader(script_source.as_bytes(), path))
        .parse_script(&Scope::new_global(), &mut script_interner)
        .map_err(parse_error)?;

    Ok(CheckedFlow {
        meta,
        meta_json,
        script_source,
        host_call_sites,
    })
}

fn extract_meta(
    module: &Module,
    interner: &Interner,
) -> Result<(Value, SourceLocation), FlowError> {
    let mut found = None;
    for item in module.items().items() {
        match item {
            ModuleItem::ImportDeclaration(_) => {
                return Err(FlowError::new(
                    "FlowMetaError",
                    "module-import-forbidden",
                    "flow scripts are Scripts and may not import modules",
                )
                .at(SourceLocation::new(1, 1)));
            }
            ModuleItem::ExportDeclaration(export) => {
                let (value, location) = meta_from_export(export, interner)?;
                if found.replace((value, location)).is_some() {
                    return Err(FlowError::new(
                        "FlowMetaError",
                        "meta-duplicate",
                        "flow script exports meta more than once",
                    )
                    .at(location));
                }
            }
            ModuleItem::StatementListItem(_) => {}
        }
    }
    found.ok_or_else(|| {
        FlowError::new(
            "FlowMetaError",
            "meta-missing",
            "flow script must declare `export const meta = { ... }`",
        )
        .at(SourceLocation::new(1, 1))
    })
}

fn meta_from_export(
    export: &ExportDeclaration,
    interner: &Interner,
) -> Result<(Value, SourceLocation), FlowError> {
    let export_location = SourceLocation::new(1, 1);
    let ExportDeclaration::Declaration(Declaration::Lexical(LexicalDeclaration::Const(variables))) =
        export
    else {
        return Err(FlowError::new(
            "FlowMetaError",
            "export-forbidden",
            "the only permitted export is `export const meta = { ... }`",
        )
        .at(export_location));
    };
    let variables = variables.as_ref();
    if variables.len() != 1 {
        return Err(FlowError::new(
            "FlowMetaError",
            "meta-nonliteral",
            "the meta export must declare only meta",
        )
        .at(export_location));
    }
    let variable = &variables[0];
    let Binding::Identifier(identifier) = variable.binding() else {
        return Err(FlowError::new(
            "FlowMetaError",
            "meta-nonliteral",
            "the meta export must use a plain identifier",
        )
        .at(export_location));
    };
    if resolve_identifier(*identifier, interner) != "meta" {
        return Err(FlowError::new(
            "FlowMetaError",
            "export-forbidden",
            "the only permitted export is named meta",
        )
        .at(location(identifier.span())));
    }
    let init = variable.init().ok_or_else(|| {
        FlowError::new(
            "FlowMetaError",
            "meta-nonliteral",
            "meta must have a pure-literal initializer",
        )
        .at(location(identifier.span()))
    })?;
    let value = literal_json(init, interner).map_err(|mut error| {
        if error.location.is_none() {
            error.location = Some(location(init.span()));
        }
        error
    })?;
    if !value.is_object() {
        return Err(FlowError::new(
            "FlowMetaError",
            "meta-nonliteral",
            "meta must be an object literal",
        )
        .at(location(init.span())));
    }
    Ok((value, location(init.span())))
}

fn literal_json(expression: &Expression, interner: &Interner) -> Result<Value, FlowError> {
    match expression.flatten() {
        Expression::Literal(literal) => match literal.kind() {
            LiteralKind::String(symbol) => {
                Ok(Value::String(interner.resolve_expect(*symbol).to_string()))
            }
            LiteralKind::Num(value) => {
                Number::from_f64(*value).map(Value::Number).ok_or_else(|| {
                    nonliteral("meta numeric literals must be finite", expression.span())
                })
            }
            LiteralKind::Int(value) => Ok(Value::Number(Number::from(*value))),
            LiteralKind::Bool(value) => Ok(Value::Bool(*value)),
            LiteralKind::Null => Ok(Value::Null),
            LiteralKind::BigInt(_) | LiteralKind::Undefined => Err(nonliteral(
                "BigInt and undefined are not JSON literals",
                expression.span(),
            )),
        },
        Expression::ArrayLiteral(array) => {
            let mut output = Vec::with_capacity(array.as_ref().len());
            for element in array.as_ref() {
                let element = element
                    .as_ref()
                    .ok_or_else(|| nonliteral("meta arrays may not contain holes", array.span()))?;
                if matches!(element, Expression::Spread(_)) {
                    return Err(nonliteral(
                        "meta arrays may not contain spreads",
                        element.span(),
                    ));
                }
                output.push(literal_json(element, interner)?);
            }
            Ok(Value::Array(output))
        }
        Expression::ObjectLiteral(object) => object_literal_json(object, interner),
        Expression::Unary(unary) if matches!(unary.op(), UnaryOp::Minus | UnaryOp::Plus) => {
            let value = literal_json(unary.target(), interner)?;
            let number = value.as_f64().ok_or_else(|| {
                nonliteral(
                    "unary signs in meta may apply only to numeric literals",
                    unary.span(),
                )
            })?;
            let signed = if unary.op() == UnaryOp::Minus {
                -number
            } else {
                number
            };
            Number::from_f64(signed)
                .map(Value::Number)
                .ok_or_else(|| nonliteral("meta numeric literals must be finite", unary.span()))
        }
        _ => Err(nonliteral(
            "meta must contain only JSON-compatible literals",
            expression.span(),
        )),
    }
}

fn object_literal_json(object: &ObjectLiteral, interner: &Interner) -> Result<Value, FlowError> {
    let mut output = Map::new();
    for property in object.properties() {
        let PropertyDefinition::Property(name, value) = property else {
            return Err(nonliteral(
                "meta objects may not use shorthand, methods, or spreads",
                object.span(),
            ));
        };
        let identifier = name
            .literal()
            .ok_or_else(|| nonliteral("meta property names may not be computed", value.span()))?;
        let key = resolve_identifier(identifier, interner);
        if output.contains_key(&key) {
            return Err(FlowError::new(
                "FlowMetaError",
                "meta-duplicate-property",
                format!("meta property {key:?} appears more than once"),
            )
            .at(location(identifier.span())));
        }
        output.insert(key, literal_json(value, interner)?);
    }
    Ok(Value::Object(output))
}

fn validate_meta(meta: &Meta, location: SourceLocation) -> Result<(), FlowError> {
    if meta.name.trim().is_empty() {
        return Err(FlowError::new(
            "FlowMetaError",
            "meta-invalid",
            "meta.name must not be empty",
        )
        .at(location));
    }
    if meta.description.trim().is_empty() {
        return Err(FlowError::new(
            "FlowMetaError",
            "meta-invalid",
            "meta.description must not be empty",
        )
        .at(location));
    }
    unique_nonempty(&meta.pools, "meta.pools", location)?;
    unique_nonempty(&meta.selectors, "meta.selectors", location)?;
    if meta.max_nodes == Some(0) {
        return Err(FlowError::new(
            "FlowMetaError",
            "meta-invalid",
            "meta.maxNodes must be positive",
        )
        .at(location));
    }
    if meta.iteration_cap == Some(0) {
        return Err(FlowError::new(
            "FlowMetaError",
            "meta-invalid",
            "meta.iterationCap must be positive",
        )
        .at(location));
    }
    jsonschema::validator_for(&meta.args_schema).map_err(|error| {
        FlowError::new(
            "FlowMetaError",
            "args-schema-invalid",
            format!("meta.argsSchema is not a valid JSON Schema: {error}"),
        )
        .at(location)
    })?;
    Ok(())
}

fn unique_nonempty(
    values: &[String],
    label: &str,
    location: SourceLocation,
) -> Result<(), FlowError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() {
            return Err(FlowError::new(
                "FlowMetaError",
                "meta-invalid",
                format!("{label} entries must not be empty"),
            )
            .at(location));
        }
        if !seen.insert(value) {
            return Err(FlowError::new(
                "FlowMetaError",
                "meta-invalid",
                format!("{label} entry {value:?} appears more than once"),
            )
            .at(location));
        }
    }
    Ok(())
}

pub(crate) fn validate_instance(
    schema: &Value,
    instance: &Value,
    name: &str,
    code: &str,
    prefix: &str,
    location: SourceLocation,
) -> Result<(), FlowError> {
    let validator = jsonschema::validator_for(schema).map_err(|error| {
        FlowError::new(
            "FlowSchemaError",
            "json-schema-invalid",
            format!("invalid JSON Schema: {error}"),
        )
        .at(location)
    })?;
    let errors = validator
        .iter_errors(instance)
        .map(|error| format!("{}: {error}", error.instance_path))
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(
            FlowError::new(name, code, format!("{prefix}: {}", errors.join("; ")))
                .at(location)
                .detail("errors", serde_json::json!(errors)),
        )
    }
}

struct DeterminismLint<'a> {
    interner: &'a Interner,
    meta: &'a Meta,
    host_call_sites: Vec<SourceLocation>,
}

impl<'ast> Visitor<'ast> for DeterminismLint<'_> {
    type BreakTy = FlowError;

    fn visit_expression(&mut self, expression: &'ast Expression) -> ControlFlow<Self::BreakTy> {
        if let Some((global, span)) = banned_expression(expression, self.interner) {
            return ControlFlow::Break(FlowError::determinism(
                global,
                format!("banned global {global} is unavailable in flow scripts"),
                location(span),
            ));
        }
        expression.visit_with(self)
    }

    fn visit_call(&mut self, call: &'ast Call) -> ControlFlow<Self::BreakTy> {
        if let Expression::Identifier(identifier) = call.function().flatten() {
            let function = resolve_identifier(*identifier, self.interner);
            self.host_call_sites.push(location(identifier.span()));
            if function == "members" {
                if let Some(Expression::Literal(literal)) =
                    call.args().first().map(Expression::flatten)
                {
                    if let LiteralKind::String(symbol) = literal.kind() {
                        let selector = self.interner.resolve_expect(*symbol).to_string();
                        if !self.meta.selectors.iter().any(|item| item == &selector) {
                            return ControlFlow::Break(
                                FlowError::new(
                                    "FlowSelectorError",
                                    "selector-undeclared",
                                    format!(
                                        "members() selector {selector:?} is absent from meta.selectors"
                                    ),
                                )
                                .at(location(literal.span()))
                                .detail("selector", selector),
                            );
                        }
                    }
                }
            }
            let spec_index = match function.as_str() {
                "job" | "drv" => Some(0),
                "claude" | "codex" | "local" | "sh" => Some(1),
                _ => None,
            };
            if let Some(index) = spec_index {
                if let Some(Expression::ObjectLiteral(object)) =
                    call.args().get(index).map(Expression::flatten)
                {
                    if let Some(pools) =
                        literal_string_array_property(object, "pools", self.interner)
                    {
                        for (pool, span) in pools {
                            if !self.meta.pools.iter().any(|declared| declared == &pool) {
                                return ControlFlow::Break(
                                    FlowError::new(
                                        "FlowPoolError",
                                        "undeclared-pool",
                                        format!(
                                            "pool {pool:?} is used by the script but absent from meta.pools"
                                        ),
                                    )
                                    .at(location(span))
                                    .detail("pool", pool),
                                );
                            }
                        }
                    }
                }
            }
        }
        call.visit_with(self)
    }
}

fn banned_expression<'a>(
    expression: &Expression,
    interner: &'a Interner,
) -> Option<(&'a str, Span)> {
    if let Expression::Identifier(identifier) = expression.flatten() {
        let name = interner.resolve_expect(identifier.sym()).to_string();
        if matches!(
            name.as_str(),
            "Date" | "WeakRef" | "FinalizationRegistry" | "eval" | "Function"
        ) {
            return Some((
                match name.as_str() {
                    "Date" => "Date",
                    "WeakRef" => "WeakRef",
                    "FinalizationRegistry" => "FinalizationRegistry",
                    "eval" => "eval",
                    "Function" => "Function",
                    _ => unreachable!(),
                },
                identifier.span(),
            ));
        }
    }
    let Expression::PropertyAccess(PropertyAccess::Simple(access)) = expression.flatten() else {
        return None;
    };
    let target = direct_identifier(access.target(), interner);
    let field = property_field_name(access.field(), interner);
    match (target.as_deref(), field.as_deref()) {
        (Some("Math"), Some("random")) => Some(("Math.random", access.span())),
        (Some("globalThis"), Some(name))
            if matches!(
                name,
                "Date" | "WeakRef" | "FinalizationRegistry" | "eval" | "Function"
            ) =>
        {
            Some((
                match name {
                    "Date" => "Date",
                    "WeakRef" => "WeakRef",
                    "FinalizationRegistry" => "FinalizationRegistry",
                    "eval" => "eval",
                    "Function" => "Function",
                    _ => unreachable!(),
                },
                access.span(),
            ))
        }
        _ => {
            if field.as_deref() == Some("random")
                && property_chain_ends_in_math(access.target(), interner)
            {
                Some(("Math.random", access.span()))
            } else {
                None
            }
        }
    }
}

fn property_chain_ends_in_math(expression: &Expression, interner: &Interner) -> bool {
    if direct_identifier(expression, interner).as_deref() == Some("Math") {
        return true;
    }
    let Expression::PropertyAccess(PropertyAccess::Simple(access)) = expression.flatten() else {
        return false;
    };
    direct_identifier(access.target(), interner).as_deref() == Some("globalThis")
        && property_field_name(access.field(), interner).as_deref() == Some("Math")
}

fn direct_identifier(expression: &Expression, interner: &Interner) -> Option<String> {
    let Expression::Identifier(identifier) = expression.flatten() else {
        return None;
    };
    Some(resolve_identifier(*identifier, interner))
}

fn property_field_name(field: &PropertyAccessField, interner: &Interner) -> Option<String> {
    match field {
        PropertyAccessField::Const(identifier) => Some(resolve_identifier(*identifier, interner)),
        PropertyAccessField::Expr(expression) => {
            let Expression::Literal(literal) = expression.flatten() else {
                return None;
            };
            match literal.kind() {
                LiteralKind::String(symbol) => Some(interner.resolve_expect(*symbol).to_string()),
                _ => None,
            }
        }
    }
}

fn literal_string_array_property(
    object: &ObjectLiteral,
    wanted: &str,
    interner: &Interner,
) -> Option<Vec<(String, Span)>> {
    for property in object.properties() {
        let PropertyDefinition::Property(name, value) = property else {
            continue;
        };
        let name = name.literal()?;
        if resolve_identifier(name, interner) != wanted {
            continue;
        }
        let Expression::ArrayLiteral(array) = value.flatten() else {
            return None;
        };
        let mut output = Vec::new();
        for item in array.as_ref() {
            let Expression::Literal(literal) = item.as_ref()?.flatten() else {
                return None;
            };
            let LiteralKind::String(symbol) = literal.kind() else {
                return None;
            };
            output.push((interner.resolve_expect(*symbol).to_string(), literal.span()));
        }
        return Some(output);
    }
    None
}

fn strip_meta_export(source: &str) -> Result<String, FlowError> {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut quote = None;
    let mut line_comment = false;
    let mut block_comment = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(active) = quote {
            if byte == b'\\' {
                index = (index + 2).min(bytes.len());
            } else {
                if byte == active {
                    quote = None;
                }
                index += 1;
            }
            continue;
        }
        if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            line_comment = true;
            index += 2;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            block_comment = true;
            index += 2;
            continue;
        }
        if bytes[index..].starts_with(b"export")
            && (index == 0 || !identifier_byte(bytes[index - 1]))
            && bytes
                .get(index + 6)
                .is_none_or(|next| !identifier_byte(*next))
        {
            let mut output = bytes.to_vec();
            output[index..index + 6].fill(b' ');
            return String::from_utf8(output).map_err(|error| {
                FlowError::new(
                    "FlowSyntaxError",
                    "script-encoding",
                    format!("script is not UTF-8 after export normalization: {error}"),
                )
                .at(SourceLocation::new(1, 1))
            });
        }
        index += 1;
    }
    Err(FlowError::new(
        "FlowMetaError",
        "meta-missing",
        "could not locate the meta export token",
    )
    .at(SourceLocation::new(1, 1)))
}

const fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

fn nonliteral(message: impl Into<String>, span: Span) -> FlowError {
    FlowError::new("FlowMetaError", "meta-nonliteral", message).at(location(span))
}

fn resolve_identifier(identifier: Identifier, interner: &Interner) -> String {
    interner.resolve_expect(identifier.sym()).to_string()
}

fn location(span: Span) -> SourceLocation {
    SourceLocation::new(span.start().line_number(), span.start().column_number())
}

fn parse_error(error: ParseError) -> FlowError {
    let location = match &error {
        ParseError::Expected { span, .. } | ParseError::Unexpected { span, .. } => {
            Some(location(*span))
        }
        ParseError::General { position, .. } => Some(SourceLocation::new(
            position.line_number(),
            position.column_number(),
        )),
        ParseError::Lex {
            err: LexError::Syntax(_, position),
        } => Some(SourceLocation::new(
            position.line_number(),
            position.column_number(),
        )),
        ParseError::AbruptEnd
        | ParseError::Lex {
            err: LexError::IO(_),
        } => None,
    }
    .unwrap_or(SourceLocation::new(1, 1));
    FlowError::syntax(error.to_string(), location)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
export const meta = {
  name: 'fixture',
  description: 'valid flow',
  pools: ['build', 'codex-window'],
  argsSchema: {
    type: 'object',
    required: ['task'],
    properties: { task: { type: 'string' } },
    additionalProperties: false
  },
  maxNodes: 12,
  iterationCap: 3,
  selectors: ['pooled-fast']
};
const x = args.task;
"#;

    #[test]
    fn pure_meta_round_trips_and_args_are_checked() {
        let args = serde_json::json!({"task": "ship"});
        let checked = check_script(
            VALID,
            Some(Path::new("fixture.js")),
            CheckOptions {
                args: Some(&args),
                ..CheckOptions::default()
            },
        )
        .unwrap();
        assert_eq!(checked.meta.name, "fixture");
        assert!(checked.script_source.contains("       const meta"));

        let bad = serde_json::json!({"task": 7});
        let error = check_script(
            VALID,
            None,
            CheckOptions {
                args: Some(&bad),
                ..CheckOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "args-schema-mismatch");
    }

    #[test]
    fn nonliteral_meta_is_rejected_at_its_expression() {
        let source = VALID.replace("maxNodes: 12", "maxNodes: Number('12')");
        let error = check_script(&source, None, CheckOptions::default()).unwrap_err();
        assert_eq!(error.code, "meta-nonliteral");
        assert!(error.location.is_some());
    }

    #[test]
    fn static_determinism_lint_names_global_and_position() {
        let source = format!("{VALID}\nMath.random();");
        let error = check_script(&source, None, CheckOptions::default()).unwrap_err();
        assert_eq!(error.name, "FlowDeterminismError");
        assert_eq!(error.details["global"], "Math.random");
        assert!(error.location.unwrap().line > 1);
    }

    #[test]
    fn statically_known_selector_and_pool_must_be_declared() {
        let selector = format!("{VALID}\nmembers('pooled-strongest');");
        assert_eq!(
            check_script(&selector, None, CheckOptions::default())
                .unwrap_err()
                .code,
            "selector-undeclared"
        );

        let pool = format!("{VALID}\njob({{argv:['true'], pools:['gpu']}});");
        assert_eq!(
            check_script(&pool, None, CheckOptions::default())
                .unwrap_err()
                .code,
            "undeclared-pool"
        );
    }

    #[test]
    fn literal_selector_cardinality_is_checked_against_the_catalog() {
        let source = format!(
            "{VALID}\nmembers('pooled-fast', {{ count: 2, diversity: 'maker' }});"
        );
        let catalog: Catalog = serde_json::from_value(serde_json::json!({
            "version": 1,
            "members": [{
                "id": "only-member",
                "family": "fixture",
                "maker": "fixture-maker",
                "classes": ["pooled-fast"],
                "adapter": "pi",
                "pools": ["worker-gpu"],
                "launch": {"model": "only-member"}
            }]
        }))
        .unwrap();

        let error = check_script(
            &source,
            None,
            CheckOptions {
                catalog: Some(&catalog),
                catalog_hash: Some("sha256:fixture"),
                ..CheckOptions::default()
            },
        )
        .unwrap_err();

        assert_eq!(error.code, "selector-insufficient-members");
        assert_eq!(error.details["selector"], "pooled-fast");
        assert_eq!(error.details["requested"], 2);
        assert_eq!(error.details["available"], 1);
    }
}
