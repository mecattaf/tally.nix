//! Conservative static rendering of checked flow scripts.
//!
//! The renderer deliberately knows only the bounded host-call surface. It does
//! not evaluate JavaScript, expand helper functions, or invent edges when a
//! value cannot be traced through literal bindings. Runtime-controlled sites
//! remain visible through a dashed decision edge instead.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use std::path::Path;

use boa_ast::declaration::{Binding, Variable};
use boa_ast::expression::access::{PropertyAccess, PropertyAccessField, SimplePropertyAccess};
use boa_ast::expression::literal::{
    LiteralKind, ObjectLiteral, PropertyDefinition, TemplateElement,
};
use boa_ast::expression::operator::Conditional;
use boa_ast::expression::{Await, Call, Expression, Identifier};
use boa_ast::function::FunctionBody;
use boa_ast::scope::Scope;
use boa_ast::statement::{DoWhileLoop, ForInLoop, ForLoop, ForOfLoop, If, Switch, WhileLoop};
use boa_ast::visitor::{VisitWith, Visitor};
use boa_ast::{Module, Span, Spanned, StatementList};
use boa_interner::Interner;
use boa_parser::{Parser, Source};

use crate::dialect::parse_error;
use crate::{check_script, CheckOptions, FlowError};

const NODE_HELPERS: &[&str] = &["job", "sh", "drv", "claude", "codex", "local"];
const COMBINATORS: &[&str] = &["parallel", "pipeline"];
const ITERATION_METHODS: &[&str] = &["map", "flatMap", "forEach"];

/// Render a checked flow script as a standalone Mermaid flowchart.
///
/// Checking happens before extraction and uses the same path and structured
/// errors as [`check_script`]. The second parse exists because the checker
/// returns normalized checked data rather than its private Boa AST/interner; no
/// script code is ever evaluated.
pub fn render_script(source: &str, path: Option<&Path>) -> Result<String, FlowError> {
    check_script(source, path, CheckOptions::default())?;

    let mut interner = Interner::default();
    let mut parser = Parser::new(Source::from_reader(source.as_bytes(), path));
    let module = parser
        .parse_module(&Scope::new_global(), &mut interner)
        .map_err(parse_error)?;
    Ok(extract_graph(&module, &interner).render())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Site {
    line: u32,
    column: u32,
    end_line: u32,
    end_column: u32,
}

impl Site {
    fn from_span(span: Span) -> Self {
        Self {
            line: span.start().line_number(),
            column: span.start().column_number(),
            end_line: span.end().line_number(),
            end_column: span.end().column_number(),
        }
    }
}

#[derive(Debug, Clone)]
enum StaticKey {
    Literal(String),
    Missing,
    Dynamic,
}

#[derive(Debug, Clone)]
struct Fanout {
    label: String,
    references: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct Node {
    site: Site,
    helper: String,
    key: StaticKey,
    pools: Option<Vec<String>>,
    fanout: Option<String>,
    references: BTreeSet<String>,
    ambiguous: bool,
    awaited: bool,
    scope_path: Vec<usize>,
    sequence: usize,
    control_path: Vec<usize>,
}

#[derive(Debug, Clone)]
struct VariableBinding {
    name: String,
    site: Site,
    scope_path: Vec<usize>,
    references: BTreeSet<String>,
    origin_sites: BTreeSet<Site>,
}

#[derive(Debug)]
struct ExtractedGraph {
    nodes: Vec<Node>,
    variables: Vec<VariableBinding>,
}

impl ExtractedGraph {
    fn render(mut self) -> String {
        self.nodes.sort_by_key(|node| node.site);
        self.nodes.dedup_by_key(|node| node.site);
        self.variables.sort_by_key(|binding| binding.site);

        let node_by_site = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.site, index))
            .collect::<BTreeMap<_, _>>();
        let variable_origins = resolve_variable_origins(&self.variables, &node_by_site);
        let mut edges = BTreeSet::new();

        for (target, node) in self.nodes.iter().enumerate() {
            for reference in &node.references {
                if let Some(binding) =
                    visible_binding(reference, node.site, &node.scope_path, &self.variables)
                {
                    for source in &variable_origins[binding] {
                        if *source != target {
                            edges.insert((*source, target));
                        }
                    }
                }
            }
        }

        // Two syntactically awaited sites in one straight-line statement
        // sequence have a real ordering dependency even when no result value is
        // consumed. Separate branch/function/loop regions never get joined.
        let mut awaited_frontier = BTreeMap::<(Vec<usize>, usize, Vec<usize>), usize>::new();
        for (target, node) in self.nodes.iter().enumerate() {
            if node.awaited {
                let region = (
                    node.scope_path.clone(),
                    node.sequence,
                    node.control_path.clone(),
                );
                if let Some(source) = awaited_frontier.insert(region, target) {
                    if source != target {
                        edges.insert((source, target));
                    }
                }
            }
        }

        let has_ambiguity = self.nodes.iter().any(|node| node.ambiguous);
        let mut output = String::from("flowchart TD\n");
        if has_ambiguity {
            output.push_str("    runtime_decided{\"runtime-decided\"}\n");
        }
        for (index, node) in self.nodes.iter().enumerate() {
            let title = match &node.key {
                StaticKey::Literal(key) => key.clone(),
                StaticKey::Missing | StaticKey::Dynamic => format!("line {}", node.site.line),
            };
            let mut details = vec![node.helper.clone()];
            if let Some(pools) = &node.pools {
                details.push(if pools.is_empty() {
                    "pools: []".to_owned()
                } else {
                    format!("pools: {}", pools.join(", "))
                });
            }
            if let Some(fanout) = &node.fanout {
                details.push(format!("×{fanout}"));
            }
            let label = format!(
                "{}<br/>{}",
                escape_mermaid_label(&title),
                escape_mermaid_label(&details.join(" · "))
            );
            output.push_str(&format!("    n{}[\"{}\"]\n", index + 1, label));
        }
        for (source, target) in edges {
            output.push_str(&format!("    n{} --> n{}\n", source + 1, target + 1));
        }
        if has_ambiguity {
            for (index, node) in self.nodes.iter().enumerate() {
                if node.ambiguous {
                    output.push_str(&format!("    runtime_decided -.-> n{}\n", index + 1));
                }
            }
        }
        output.pop();
        output
    }
}

fn extract_graph(module: &Module, interner: &Interner) -> ExtractedGraph {
    let mut extractor = GraphExtractor {
        interner,
        nodes: Vec::new(),
        variables: Vec::new(),
        scope_path: vec![0],
        next_scope: 0,
        sequence: 0,
        next_sequence: 0,
        control_path: Vec::new(),
        control_references: BTreeSet::new(),
        next_control: 0,
        fanout: None,
        settle_depth: 0,
        await_depth: 0,
    };
    let _ = module.visit_with(&mut extractor);
    ExtractedGraph {
        nodes: extractor.nodes,
        variables: extractor.variables,
    }
}

struct GraphExtractor<'a> {
    interner: &'a Interner,
    nodes: Vec<Node>,
    variables: Vec<VariableBinding>,
    scope_path: Vec<usize>,
    next_scope: usize,
    sequence: usize,
    next_sequence: usize,
    control_path: Vec<usize>,
    control_references: BTreeSet<String>,
    next_control: usize,
    fanout: Option<Fanout>,
    settle_depth: usize,
    await_depth: usize,
}

impl GraphExtractor<'_> {
    fn enter_control(&mut self, references: BTreeSet<String>) -> ControlSnapshot {
        self.next_control += 1;
        let snapshot = ControlSnapshot {
            path_len: self.control_path.len(),
            references: self.control_references.clone(),
        };
        self.control_path.push(self.next_control);
        self.control_references.extend(references);
        snapshot
    }

    fn leave_control(&mut self, snapshot: ControlSnapshot) {
        self.control_path.truncate(snapshot.path_len);
        self.control_references = snapshot.references;
    }

    fn visit_loop_body<'ast>(
        &mut self,
        body: &'ast boa_ast::Statement,
        source: Option<&'ast Expression>,
    ) -> ControlFlow<()> {
        let references = source.map_or_else(BTreeSet::new, |source| {
            referenced_identifiers(source, self.interner)
        });
        let saved_fanout = self.fanout.clone();
        if let Some(source) =
            source.and_then(|source| fanout_from_expression(source, self.interner))
        {
            self.fanout = Some(source);
        }
        let snapshot = self.enter_control(references);
        let result = self.visit_statement(body);
        self.leave_control(snapshot);
        self.fanout = saved_fanout;
        result
    }
}

#[derive(Debug)]
struct ControlSnapshot {
    path_len: usize,
    references: BTreeSet<String>,
}

impl<'ast> Visitor<'ast> for GraphExtractor<'_> {
    type BreakTy = ();

    fn visit_function_body(&mut self, body: &'ast FunctionBody) -> ControlFlow<Self::BreakTy> {
        self.next_scope += 1;
        self.next_sequence += 1;
        let saved_sequence = self.sequence;
        let saved_await_depth = self.await_depth;
        self.scope_path.push(self.next_scope);
        self.sequence = self.next_sequence;
        // An outer `await parallel(...)` does not make a thunk's host call an
        // awaited expression. Fanout/settle/control context intentionally does
        // cross this boundary because it describes how the thunk is invoked.
        self.await_depth = 0;
        let result = body.visit_with(self);
        self.await_depth = saved_await_depth;
        self.sequence = saved_sequence;
        self.scope_path.pop();
        result
    }

    fn visit_statement_list(&mut self, list: &'ast StatementList) -> ControlFlow<Self::BreakTy> {
        self.next_sequence += 1;
        let saved = self.sequence;
        self.sequence = self.next_sequence;
        let result = list.visit_with(self);
        self.sequence = saved;
        result
    }

    fn visit_variable(&mut self, variable: &'ast Variable) -> ControlFlow<Self::BreakTy> {
        if let (Binding::Identifier(identifier), Some(init)) = (variable.binding(), variable.init())
        {
            let mut origin_sites = BTreeSet::new();
            if let Some(call) = direct_root_call(init) {
                if direct_call_name(call, self.interner)
                    .is_some_and(|name| NODE_HELPERS.contains(&name.as_str()))
                {
                    origin_sites.insert(Site::from_span(call.span()));
                } else if direct_call_name(call, self.interner)
                    .is_some_and(|name| COMBINATORS.contains(&name.as_str()))
                {
                    let mut collector = NodeSiteCollector {
                        interner: self.interner,
                        sites: BTreeSet::new(),
                    };
                    let _ = init.visit_with(&mut collector);
                    origin_sites = collector.sites;
                }
            }
            self.variables.push(VariableBinding {
                name: resolve_identifier(*identifier, self.interner),
                site: Site::from_span(identifier.span()),
                scope_path: self.scope_path.clone(),
                references: referenced_identifiers(init, self.interner),
                origin_sites,
            });
        }
        variable.visit_with(self)
    }

    fn visit_call(&mut self, call: &'ast Call) -> ControlFlow<Self::BreakTy> {
        let direct_helper = direct_call_name(call, self.interner);
        if let Some(helper) = direct_helper
            .as_ref()
            .filter(|helper| NODE_HELPERS.contains(&helper.as_str()))
        {
            let (key, pools) = node_annotations(call, helper, self.interner);
            let mut references = BTreeSet::new();
            for argument in call.args() {
                references.extend(referenced_identifiers(argument, self.interner));
            }
            references.extend(self.control_references.iter().cloned());
            if let Some(fanout) = &self.fanout {
                references.extend(fanout.references.iter().cloned());
            }
            let own_settle = call.args().get(1).is_some_and(|options| {
                settle_expression_is_runtime_controlled(options, self.interner)
            });
            let ambiguous = matches!(key, StaticKey::Dynamic)
                || !self.control_path.is_empty()
                || self.fanout.is_some()
                || self.settle_depth > 0
                || own_settle;
            self.nodes.push(Node {
                site: Site::from_span(call.span()),
                helper: helper.clone(),
                key,
                pools,
                fanout: self.fanout.as_ref().map(|fanout| fanout.label.clone()),
                references,
                ambiguous,
                awaited: self.await_depth > 0,
                scope_path: self.scope_path.clone(),
                sequence: self.sequence,
                control_path: self.control_path.clone(),
            });
            return call.visit_with(self);
        }

        let Some(helper) = direct_helper.or_else(|| called_method_name(call, self.interner)) else {
            return call.visit_with(self);
        };

        let mapped = mapped_collection(call, &helper, self.interner);
        let pipeline = (helper == "pipeline")
            .then(|| call.args().first())
            .flatten()
            .and_then(|items| fanout_from_expression(items, self.interner));
        let new_fanout = mapped.or(pipeline);
        let combinator_settle = match helper.as_str() {
            "parallel" => call.args().get(1).is_some_and(|options| {
                settle_expression_is_runtime_controlled(options, self.interner)
            }),
            "pipeline" => call.args().iter().skip(1).any(|argument| {
                object_literal(argument)
                    .is_some_and(|object| settle_is_runtime_controlled(object, self.interner))
            }),
            _ => false,
        };
        let saved_fanout = self.fanout.clone();
        let mut control = None;
        if let Some(fanout) = new_fanout {
            let references = fanout.references.clone();
            self.fanout = Some(fanout);
            control = Some(self.enter_control(references));
        }
        if combinator_settle {
            self.settle_depth += 1;
        }
        let result = call.visit_with(self);
        if combinator_settle {
            self.settle_depth -= 1;
        }
        if let Some(snapshot) = control {
            self.leave_control(snapshot);
        }
        self.fanout = saved_fanout;
        result
    }

    fn visit_await(&mut self, await_expression: &'ast Await) -> ControlFlow<Self::BreakTy> {
        self.await_depth += 1;
        let result = self.visit_expression(await_expression.target());
        self.await_depth -= 1;
        result
    }

    fn visit_if(&mut self, statement: &'ast If) -> ControlFlow<Self::BreakTy> {
        self.visit_expression(statement.cond())?;
        let references = referenced_identifiers(statement.cond(), self.interner);
        let body = self.enter_control(references.clone());
        self.visit_statement(statement.body())?;
        self.leave_control(body);
        if let Some(alternate) = statement.else_node() {
            let alternate_snapshot = self.enter_control(references);
            self.visit_statement(alternate)?;
            self.leave_control(alternate_snapshot);
        }
        ControlFlow::Continue(())
    }

    fn visit_conditional(&mut self, conditional: &'ast Conditional) -> ControlFlow<Self::BreakTy> {
        self.visit_expression(conditional.condition())?;
        let references = referenced_identifiers(conditional.condition(), self.interner);
        let truthy = self.enter_control(references.clone());
        self.visit_expression(conditional.if_true())?;
        self.leave_control(truthy);
        let falsy = self.enter_control(references);
        self.visit_expression(conditional.if_false())?;
        self.leave_control(falsy);
        ControlFlow::Continue(())
    }

    fn visit_for_of_loop(&mut self, statement: &'ast ForOfLoop) -> ControlFlow<Self::BreakTy> {
        self.visit_iterable_loop_initializer(statement.initializer())?;
        self.visit_expression(statement.iterable())?;
        self.visit_loop_body(statement.body(), Some(statement.iterable()))
    }

    fn visit_for_in_loop(&mut self, statement: &'ast ForInLoop) -> ControlFlow<Self::BreakTy> {
        self.visit_iterable_loop_initializer(statement.initializer())?;
        self.visit_expression(statement.target())?;
        self.visit_loop_body(statement.body(), Some(statement.target()))
    }

    fn visit_for_loop(&mut self, statement: &'ast ForLoop) -> ControlFlow<Self::BreakTy> {
        if let Some(initializer) = statement.init() {
            self.visit_for_loop_initializer(initializer)?;
        }
        if let Some(condition) = statement.condition() {
            self.visit_expression(condition)?;
        }
        if let Some(final_expression) = statement.final_expr() {
            self.visit_expression(final_expression)?;
        }
        let source = statement
            .condition()
            .and_then(|condition| loop_collection(condition, self.interner));
        let saved_fanout = self.fanout.clone();
        if let Some(source) = source.clone() {
            self.fanout = Some(source);
        }
        let references = statement
            .condition()
            .map_or_else(BTreeSet::new, |condition| {
                referenced_identifiers(condition, self.interner)
            });
        let snapshot = self.enter_control(references);
        let result = self.visit_statement(statement.body());
        self.leave_control(snapshot);
        self.fanout = saved_fanout;
        result
    }

    fn visit_while_loop(&mut self, statement: &'ast WhileLoop) -> ControlFlow<Self::BreakTy> {
        self.visit_expression(statement.condition())?;
        let source = loop_collection(statement.condition(), self.interner);
        let saved_fanout = self.fanout.clone();
        if let Some(source) = source {
            self.fanout = Some(source);
        }
        let references = referenced_identifiers(statement.condition(), self.interner);
        let snapshot = self.enter_control(references);
        let result = self.visit_statement(statement.body());
        self.leave_control(snapshot);
        self.fanout = saved_fanout;
        result
    }

    fn visit_do_while_loop(&mut self, statement: &'ast DoWhileLoop) -> ControlFlow<Self::BreakTy> {
        let source = loop_collection(statement.cond(), self.interner);
        let saved_fanout = self.fanout.clone();
        if let Some(source) = source {
            self.fanout = Some(source);
        }
        let references = referenced_identifiers(statement.cond(), self.interner);
        let snapshot = self.enter_control(references);
        self.visit_statement(statement.body())?;
        self.leave_control(snapshot);
        self.fanout = saved_fanout;
        self.visit_expression(statement.cond())
    }

    fn visit_switch(&mut self, statement: &'ast Switch) -> ControlFlow<Self::BreakTy> {
        self.visit_expression(statement.val())?;
        let switched = referenced_identifiers(statement.val(), self.interner);
        for case in statement.cases() {
            if let Some(condition) = case.condition() {
                self.visit_expression(condition)?;
            }
            let mut references = switched.clone();
            if let Some(condition) = case.condition() {
                references.extend(referenced_identifiers(condition, self.interner));
            }
            let snapshot = self.enter_control(references);
            self.visit_statement_list(case.body())?;
            self.leave_control(snapshot);
        }
        ControlFlow::Continue(())
    }
}

fn node_annotations(
    call: &Call,
    helper: &str,
    interner: &Interner,
) -> (StaticKey, Option<Vec<String>>) {
    if helper == "drv" {
        return (StaticKey::Missing, Some(vec!["build".to_owned()]));
    }
    let fixed_pools = match helper {
        "claude" => Some(vec!["claude-window".to_owned()]),
        "codex" => Some(vec!["codex-window".to_owned()]),
        _ => None,
    };
    let spec_index = usize::from(!matches!(helper, "job"));
    let Some(spec) = call.args().get(spec_index) else {
        return (StaticKey::Missing, fixed_pools);
    };
    let Some(object) = object_literal(spec) else {
        return (StaticKey::Dynamic, fixed_pools);
    };
    let key = static_key(object, interner);
    let pools = fixed_pools.or_else(|| literal_string_array(object, "pools", interner));
    (key, pools)
}

fn static_key(object: &ObjectLiteral, interner: &Interner) -> StaticKey {
    let mut key = StaticKey::Missing;
    for property in object.properties() {
        match property {
            PropertyDefinition::Property(name, value) => {
                let Some(name) = name.literal() else {
                    key = StaticKey::Dynamic;
                    continue;
                };
                if resolve_identifier(name, interner) == "key" {
                    key = literal_string(value, interner)
                        .map_or(StaticKey::Dynamic, StaticKey::Literal);
                }
            }
            PropertyDefinition::IdentifierReference(identifier) => {
                if resolve_identifier(*identifier, interner) == "key" {
                    key = StaticKey::Dynamic;
                }
            }
            PropertyDefinition::CoverInitializedName(identifier, _) => {
                if resolve_identifier(*identifier, interner) == "key" {
                    key = StaticKey::Dynamic;
                }
            }
            PropertyDefinition::MethodDefinition(method) => match method.name().literal() {
                Some(name) if resolve_identifier(name, interner) == "key" => {
                    key = StaticKey::Dynamic;
                }
                None => key = StaticKey::Dynamic,
                Some(_) => {}
            },
            PropertyDefinition::SpreadObject(_) => key = StaticKey::Dynamic,
        }
    }
    key
}

fn literal_string_array(
    object: &ObjectLiteral,
    wanted: &str,
    interner: &Interner,
) -> Option<Vec<String>> {
    let mut output = None;
    for property in object.properties() {
        match property {
            PropertyDefinition::Property(name, value) => {
                let Some(name) = name.literal() else {
                    output = None;
                    continue;
                };
                if resolve_identifier(name, interner) != wanted {
                    continue;
                }
                let Expression::ArrayLiteral(array) = value.flatten() else {
                    output = None;
                    continue;
                };
                let values = array
                    .as_ref()
                    .iter()
                    .map(|item| literal_string(item.as_ref()?, interner))
                    .collect::<Option<Vec<_>>>();
                output = values;
            }
            PropertyDefinition::IdentifierReference(identifier)
            | PropertyDefinition::CoverInitializedName(identifier, _) => {
                if resolve_identifier(*identifier, interner) == wanted {
                    output = None;
                }
            }
            PropertyDefinition::MethodDefinition(method) => match method.name().literal() {
                Some(name) if resolve_identifier(name, interner) == wanted => output = None,
                None => output = None,
                Some(_) => {}
            },
            PropertyDefinition::SpreadObject(_) => output = None,
        }
    }
    output
}

fn literal_string(expression: &Expression, interner: &Interner) -> Option<String> {
    match expression.flatten() {
        Expression::Literal(literal) => {
            let LiteralKind::String(symbol) = literal.kind() else {
                return None;
            };
            Some(interner.resolve_expect(*symbol).to_string())
        }
        Expression::TemplateLiteral(template) => {
            let mut output = String::new();
            for element in template.elements() {
                let TemplateElement::String(symbol) = element else {
                    return None;
                };
                output.push_str(&interner.resolve_expect(*symbol).to_string());
            }
            Some(output)
        }
        _ => None,
    }
}

fn object_literal(expression: &Expression) -> Option<&ObjectLiteral> {
    let Expression::ObjectLiteral(object) = expression.flatten() else {
        return None;
    };
    Some(object)
}

fn settle_is_runtime_controlled(object: &ObjectLiteral, interner: &Interner) -> bool {
    let mut controlled = false;
    for property in object.properties() {
        match property {
            PropertyDefinition::Property(name, value) => {
                let Some(name) = name.literal() else {
                    controlled = true;
                    continue;
                };
                if resolve_identifier(name, interner) == "settle" {
                    controlled = !matches!(
                        value.flatten(),
                        Expression::Literal(literal)
                            if matches!(literal.kind(), LiteralKind::Bool(false))
                    );
                }
            }
            PropertyDefinition::IdentifierReference(identifier)
            | PropertyDefinition::CoverInitializedName(identifier, _) => {
                if resolve_identifier(*identifier, interner) == "settle" {
                    controlled = true;
                }
            }
            PropertyDefinition::MethodDefinition(method) => match method.name().literal() {
                Some(name) if resolve_identifier(name, interner) == "settle" => controlled = true,
                None => controlled = true,
                Some(_) => {}
            },
            PropertyDefinition::SpreadObject(_) => controlled = true,
        }
    }
    controlled
}

fn settle_expression_is_runtime_controlled(expression: &Expression, interner: &Interner) -> bool {
    object_literal(expression).is_none_or(|object| settle_is_runtime_controlled(object, interner))
}

fn direct_root_call(expression: &Expression) -> Option<&Call> {
    match expression.flatten() {
        Expression::Await(await_expression) => direct_root_call(await_expression.target()),
        Expression::Call(call) => Some(call),
        _ => None,
    }
}

fn direct_call_name(call: &Call, interner: &Interner) -> Option<String> {
    let Expression::Identifier(identifier) = call.function().flatten() else {
        return None;
    };
    Some(resolve_identifier(*identifier, interner))
}

fn called_method_name(call: &Call, interner: &Interner) -> Option<String> {
    let Expression::PropertyAccess(PropertyAccess::Simple(access)) = call.function().flatten()
    else {
        return None;
    };
    property_field_name(access.field(), interner)
}

fn mapped_collection(call: &Call, helper: &str, interner: &Interner) -> Option<Fanout> {
    if !ITERATION_METHODS.contains(&helper) {
        return None;
    }
    let Expression::PropertyAccess(PropertyAccess::Simple(access)) = call.function().flatten()
    else {
        return None;
    };
    fanout_from_expression(access.target(), interner)
}

fn fanout_from_expression(expression: &Expression, interner: &Interner) -> Option<Fanout> {
    let label = expression_path(expression, interner)?;
    Some(Fanout {
        label,
        references: referenced_identifiers(expression, interner),
    })
}

fn expression_path(expression: &Expression, interner: &Interner) -> Option<String> {
    match expression.flatten() {
        Expression::Identifier(identifier) => Some(resolve_identifier(*identifier, interner)),
        Expression::PropertyAccess(PropertyAccess::Simple(access)) => {
            simple_access_path(access, interner)
        }
        // Preserve the collection across non-mutating selection helpers, so
        // `args.inputs.slice(...).map(...)` still renders as `×args.inputs`.
        Expression::Call(call) => {
            let Expression::PropertyAccess(PropertyAccess::Simple(access)) =
                call.function().flatten()
            else {
                return None;
            };
            let field = property_field_name(access.field(), interner)?;
            matches!(field.as_str(), "slice" | "filter" | "values")
                .then(|| expression_path(access.target(), interner))
                .flatten()
        }
        _ => None,
    }
}

fn simple_access_path(access: &SimplePropertyAccess, interner: &Interner) -> Option<String> {
    let target = expression_path(access.target(), interner)?;
    let field = property_field_name(access.field(), interner)?;
    Some(format!("{target}.{field}"))
}

fn property_field_name(field: &PropertyAccessField, interner: &Interner) -> Option<String> {
    match field {
        PropertyAccessField::Const(identifier) => Some(resolve_identifier(*identifier, interner)),
        PropertyAccessField::Expr(expression) => literal_string(expression, interner),
    }
}

fn loop_collection(expression: &Expression, interner: &Interner) -> Option<Fanout> {
    let mut collector = LengthCollectionCollector {
        interner,
        collections: Vec::new(),
    };
    let _ = expression.visit_with(&mut collector);
    collector
        .collections
        .into_iter()
        .max_by_key(|fanout| fanout.label.len())
}

struct LengthCollectionCollector<'a> {
    interner: &'a Interner,
    collections: Vec<Fanout>,
}

impl<'ast> Visitor<'ast> for LengthCollectionCollector<'_> {
    type BreakTy = ();

    fn visit_property_access(
        &mut self,
        access: &'ast PropertyAccess,
    ) -> ControlFlow<Self::BreakTy> {
        if let PropertyAccess::Simple(access) = access {
            if property_field_name(access.field(), self.interner).as_deref() == Some("length") {
                if let Some(fanout) = fanout_from_expression(access.target(), self.interner) {
                    self.collections.push(fanout);
                }
            }
        }
        access.visit_with(self)
    }
}

fn referenced_identifiers(expression: &Expression, interner: &Interner) -> BTreeSet<String> {
    let mut collector = ReferenceCollector {
        interner,
        references: BTreeSet::new(),
    };
    let _ = expression.visit_with(&mut collector);
    collector.references
}

struct ReferenceCollector<'a> {
    interner: &'a Interner,
    references: BTreeSet<String>,
}

impl<'ast> Visitor<'ast> for ReferenceCollector<'_> {
    type BreakTy = ();

    fn visit_identifier(&mut self, identifier: &'ast Identifier) -> ControlFlow<Self::BreakTy> {
        self.references
            .insert(resolve_identifier(*identifier, self.interner));
        ControlFlow::Continue(())
    }

    fn visit_binding(&mut self, _binding: &'ast Binding) -> ControlFlow<Self::BreakTy> {
        ControlFlow::Continue(())
    }

    fn visit_function_body(&mut self, _body: &'ast FunctionBody) -> ControlFlow<Self::BreakTy> {
        // A callback's body is a later execution context, not an input value of
        // the expression that creates it.
        ControlFlow::Continue(())
    }
}

struct NodeSiteCollector<'a> {
    interner: &'a Interner,
    sites: BTreeSet<Site>,
}

impl<'ast> Visitor<'ast> for NodeSiteCollector<'_> {
    type BreakTy = ();

    fn visit_call(&mut self, call: &'ast Call) -> ControlFlow<Self::BreakTy> {
        if direct_call_name(call, self.interner)
            .is_some_and(|name| NODE_HELPERS.contains(&name.as_str()))
        {
            self.sites.insert(Site::from_span(call.span()));
        }
        call.visit_with(self)
    }
}

fn resolve_variable_origins(
    variables: &[VariableBinding],
    nodes: &BTreeMap<Site, usize>,
) -> Vec<BTreeSet<usize>> {
    let mut resolved = vec![BTreeSet::new(); variables.len()];
    for (index, binding) in variables.iter().enumerate() {
        for site in &binding.origin_sites {
            if let Some(node) = nodes.get(site) {
                resolved[index].insert(*node);
            }
        }
        if resolved[index].is_empty() {
            for reference in &binding.references {
                if let Some(source) = visible_binding_before(
                    reference,
                    binding.site,
                    &binding.scope_path,
                    variables,
                    index,
                ) {
                    let origins = resolved[source].clone();
                    resolved[index].extend(origins);
                }
            }
        }
    }
    resolved
}

fn visible_binding(
    name: &str,
    use_site: Site,
    use_scope: &[usize],
    variables: &[VariableBinding],
) -> Option<usize> {
    visible_binding_before(name, use_site, use_scope, variables, variables.len())
}

fn visible_binding_before(
    name: &str,
    use_site: Site,
    use_scope: &[usize],
    variables: &[VariableBinding],
    limit: usize,
) -> Option<usize> {
    variables
        .iter()
        .take(limit)
        .enumerate()
        .filter(|(_, binding)| {
            binding.name == name
                && binding.site < use_site
                && use_scope.starts_with(&binding.scope_path)
        })
        .max_by_key(|(_, binding)| (binding.scope_path.len(), binding.site))
        .map(|(index, _)| index)
}

fn resolve_identifier(identifier: Identifier, interner: &Interner) -> String {
    interner.resolve_expect(identifier.sym()).to_string()
}

fn escape_mermaid_label(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: &str = r#"
export const meta = {
  name: "render-fixture",
  description: "static render fixture",
  pools: ["worker-cpu", "codex-window"],
  argsSchema: {
    type: "object",
    required: ["inputs"],
    properties: { inputs: { type: "array", items: { type: "string" } } }
  },
  iterationCap: 4,
  selectors: []
};
"#;

    #[test]
    fn renders_literal_nodes_dependencies_fanout_and_runtime_control() {
        let source = format!(
            r#"{META}
(async () => {{
  const source = await sh(["produce"], {{
    key: "source",
    pools: ["worker-cpu"]
  }});
  const nodes = await parallel(
    args.inputs.map(input => () => codex(
      `process ${{input}} from ${{source.result.path}}`,
      {{ key: `worker-${{input}}` }}
    )),
    {{ settle: true }}
  );
  if (nodes.some(outcome => outcome.ok)) {{
    await job({{
      key: "finish",
      pools: ["worker-cpu"],
      prompt: source.result.path
    }});
  }}
}})();
"#
        );
        let rendered = render_script(&source, Some(Path::new("fixture.js"))).unwrap();
        assert!(rendered.starts_with("flowchart TD\n"), "{rendered}");
        assert!(
            rendered.contains("source<br/>sh · pools: worker-cpu"),
            "{rendered}"
        );
        assert!(
            rendered.contains("codex · pools: codex-window · ×args.inputs"),
            "{rendered}"
        );
        assert!(
            rendered.contains("finish<br/>job · pools: worker-cpu"),
            "{rendered}"
        );
        assert!(rendered.contains("n1 --> n2"), "{rendered}");
        assert!(rendered.contains("n1 --> n3"), "{rendered}");
        assert!(rendered.contains("n2 --> n3"), "{rendered}");
        assert!(rendered.contains("runtime_decided -.-> n2"), "{rendered}");
        assert!(rendered.contains("runtime_decided -.-> n3"), "{rendered}");
    }

    #[test]
    fn rendering_never_evaluates_the_checked_script() {
        let source = format!(
            r#"{META}
throw new Error("render must not execute this");
sh(["unreachable"], {{ key: "still-visible", pools: ["worker-cpu"] }});
"#
        );
        let rendered = render_script(&source, None).unwrap();
        assert!(rendered.contains("still-visible<br/>sh"), "{rendered}");
    }

    #[test]
    fn rendering_keeps_checker_failures() {
        let source = format!("{META}\nMath.random();\n");
        let error = render_script(&source, None).unwrap_err();
        assert_eq!(error.code, "determinism-violation");
    }

    #[test]
    fn later_spreads_stay_dynamic_and_later_literals_restore_certainty() {
        let source = format!(
            r#"{META}
args.choose
  ? sh(["first"], {{
      key: "overridden",
      pools: [],
      ...args.options
    }})
  : sh(["second"], {{
      ...args.options,
      key: `after-spread`,
      pools: []
    }});
"#
        );
        let rendered = render_script(&source, None).unwrap();
        assert!(rendered.contains("line 16<br/>sh"), "{rendered}");
        assert!(!rendered.contains("overridden<br/>"), "{rendered}");
        assert!(
            rendered.contains("after-spread<br/>sh · pools: []"),
            "{rendered}"
        );
        assert!(rendered.contains("runtime_decided -.-> n1"), "{rendered}");
        assert!(rendered.contains("runtime_decided -.-> n2"), "{rendered}");
    }
}
