use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use interloko_grammar::{self, GrammarAction, GrammarActionTerm};

pub(super) use interloko_grammar::{
    Parse as InterpretedParse, ParseError as InterpretedParseError,
    RuleNode as InterpretedRuleNode, SourceAuthoredRule,
};

use crate::source_data::{self, SourceData};
use crate::syntax::Span;

use super::{
    FrontendBinaryOp, FrontendExpression, FrontendForm, FrontendSlot, StagedRuleCount,
    frontend_expression_source_data, frontend_form_source_data, frontend_slot_source_data,
    grammar_action_source_data, source_span_data, valid_calculator_function_name,
    valid_record_field_name, valid_record_type_fragment, valid_type_name,
};

const LOKO_RESERVED_WORDS: &[&str] = &[
    "define", "function", "class", "slot", "constant", "generic", "method", "let", "if", "then",
    "else", "handle", "with", "restart", "end",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum GrammarActionValue {
    Text {
        value: String,
        span: Span,
    },
    FrontendSlot(FrontendSlot),
    FrontendForm(FrontendForm),
    FrontendExpression(FrontendExpression),
    FrontendExpressionList(Vec<FrontendExpression>),
    BinaryTail {
        op: FrontendBinaryOp,
        rhs: FrontendExpression,
        span: Span,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GrammarActionOutput {
    pub(super) action: GrammarAction,
    pub(super) value: GrammarActionValue,
    pub(super) data: SourceData,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GrammarActionError {
    pub(super) span: Span,
    pub(super) expected: Box<str>,
    pub(super) field: Option<Box<str>>,
    pub(super) action: Option<Box<str>>,
    pub(super) arg: Option<Box<str>>,
    pub(super) rule: Option<Box<str>>,
}

impl GrammarActionError {
    fn new(span: impl Into<Span>, expected: impl Into<String>) -> Self {
        Self {
            span: span.into(),
            expected: expected.into().into_boxed_str(),
            field: None,
            action: None,
            arg: None,
            rule: None,
        }
    }

    fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into().into_boxed_str());
        self
    }

    fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.arg = Some(arg.into().into_boxed_str());
        self
    }

    fn with_action_context(mut self, action: &str, rule: &str) -> Self {
        self.action
            .get_or_insert_with(|| action.to_string().into_boxed_str());
        self.rule
            .get_or_insert_with(|| rule.to_string().into_boxed_str());
        self
    }
}

pub(super) fn parse_source_authored_grammar(source: &str) -> Vec<SourceAuthoredRule> {
    interloko_grammar::parse_grammar_source(source)
        .into_iter()
        .map(|rule| {
            if rule.name == "name" {
                rule.with_excluded_text(
                    LOKO_RESERVED_WORDS
                        .iter()
                        .map(|word| (*word).to_string())
                        .collect(),
                )
            } else {
                rule
            }
        })
        .collect()
}

pub(super) fn interpret_region_result(
    rules: &[SourceAuthoredRule],
    root_rule: &str,
    source: &str,
    source_offset: usize,
) -> Result<InterpretedParse, InterpretedParseError> {
    interloko_grammar::interpret_region(rules, root_rule, source, source_offset)
}

pub(super) fn interpret_rule_error(
    rules: &[SourceAuthoredRule],
    rule_name: &str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> InterpretedParseError {
    interloko_grammar::interpret_rule_error(rules, rule_name, source, position, source_offset)
}

pub(super) fn evaluate_rule_action(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
) -> Result<Option<GrammarActionOutput>, GrammarActionError> {
    let Some(action) = rules
        .iter()
        .find(|rule| rule.name == node.rule_name)
        .and_then(|rule| rule.action.clone())
    else {
        return Ok(None);
    };
    let value = match action.name.as_str() {
        "emit-class" => {
            evaluate_emit_class(rules, node, &action).map(GrammarActionValue::FrontendForm)
        }
        "emit-slot" => evaluate_emit_slot(node, &action).map(GrammarActionValue::FrontendSlot),
        "emit-function" => {
            evaluate_emit_function(rules, node, &action).map(GrammarActionValue::FrontendForm)
        }
        "emit-integer" => {
            evaluate_emit_integer(node, &action).map(GrammarActionValue::FrontendExpression)
        }
        "emit-name" => {
            evaluate_emit_name(node, &action).map(GrammarActionValue::FrontendExpression)
        }
        "emit-call" => {
            evaluate_emit_call(rules, node, &action).map(GrammarActionValue::FrontendExpression)
        }
        "emit-argument-list" => evaluate_emit_argument_list(rules, node, &action)
            .map(GrammarActionValue::FrontendExpressionList),
        "emit-argument-tail" => action_expression_arg(rules, node, &action, "value")
            .map(|value| GrammarActionValue::FrontendExpressionList(Vec::from([value]))),
        "emit-binary-tail" => (|| {
            let op = action_text_arg(node, &action, "op")?;
            let rhs = action_expression_arg(rules, node, &action, "rhs")?;
            Ok(GrammarActionValue::BinaryTail {
                op: binary_op_from_text(&op.value).ok_or_else(|| {
                    GrammarActionError::new(op.span, "binary operator").with_arg("op")
                })?,
                span: Span::new(op.span.start, expr_span(&rhs).end),
                rhs,
            })
        })(),
        "fold-binary" => {
            evaluate_fold_binary(rules, node, &action).map(GrammarActionValue::FrontendExpression)
        }
        _ => Err(GrammarActionError::new(
            node.span,
            format!("known grammar action `{}`", action.name),
        )),
    }
    .map_err(|error| error.with_action_context(&action.name, &node.rule_name))?;
    Ok(Some(GrammarActionOutput {
        data: grammar_action_output_source_data(&action, &value),
        action,
        value,
    }))
}

fn evaluate_emit_class(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
    action: &GrammarAction,
) -> Result<FrontendForm, GrammarActionError> {
    let name = action_text_arg(node, action, "name")?;
    let class_name = class_name_from_action_text(&name.value)
        .ok_or_else(|| GrammarActionError::new(name.span, "valid class name").with_arg("name"))?;
    let superclass = action_text_arg(node, action, "superclass")
        .map(|value| value.value)
        .unwrap_or_else(|_| "<object>".to_string());
    let slots = action_list_arg(rules, node, action, "slots")?
        .into_iter()
        .map(|value| match value {
            GrammarActionValue::FrontendSlot(slot) => Ok(slot),
            _ => Err(GrammarActionError::new(node.span, "slot action output")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (index, slot) in slots.iter().enumerate() {
        if slots[..index].iter().any(|prior| prior.name == slot.name) {
            return Err(
                GrammarActionError::new(slot.source_span, "unique record field")
                    .with_field(slot.name.clone()),
            );
        }
    }
    Ok(FrontendForm::class(
        class_name,
        superclass,
        slots,
        node.span.into(),
    ))
}

fn evaluate_emit_slot(
    node: &InterpretedRuleNode,
    action: &GrammarAction,
) -> Result<FrontendSlot, GrammarActionError> {
    let name = action_text_arg(node, action, "name")?;
    let type_name = action_text_arg(node, action, "type")?;
    if !valid_record_field_name(name.value.trim()) {
        return Err(GrammarActionError::new(name.span, "valid record field")
            .with_arg("name")
            .with_field(name.value.clone()));
    }
    if !valid_type_name(type_name.value.trim()) {
        return Err(GrammarActionError::new(type_name.span, "field type name")
            .with_arg("type")
            .with_field(name.value.clone()));
    }
    let init_keyword = action_text_arg(node, action, "init-keyword")
        .map(|value| value.value)
        .ok();
    let required = action_bool_arg(action, "required-init-keyword").unwrap_or(false);
    let mut slot = FrontendSlot::new(name.value.trim(), type_name.value.trim(), node.span.into());
    if let Some(init_keyword) = init_keyword {
        slot = slot.with_init_keyword(init_keyword.trim(), required);
    }
    Ok(slot)
}

fn evaluate_emit_function(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
    action: &GrammarAction,
) -> Result<FrontendForm, GrammarActionError> {
    let name = action_text_arg(node, action, "name")?;
    if !valid_calculator_function_name(name.value.trim()) {
        return Err(GrammarActionError::new(name.span, "`name = expression`").with_arg("name"));
    }
    let body = action_expression_arg(rules, node, action, "body")?;
    Ok(FrontendForm::function(
        name.value.trim(),
        body,
        node.span.into(),
    ))
}

fn evaluate_emit_integer(
    node: &InterpretedRuleNode,
    action: &GrammarAction,
) -> Result<FrontendExpression, GrammarActionError> {
    let value = action_text_arg(node, action, "value")?;
    Ok(FrontendExpression::Integer {
        value: value.value.parse().map_err(|_| {
            GrammarActionError::new(value.span, "integer literal").with_arg("value")
        })?,
        source_span: node.span.into(),
    })
}

fn evaluate_emit_name(
    node: &InterpretedRuleNode,
    action: &GrammarAction,
) -> Result<FrontendExpression, GrammarActionError> {
    let name = action_text_arg(node, action, "name")?;
    Ok(FrontendExpression::Name {
        name: name.value,
        source_span: node.span.into(),
    })
}

fn evaluate_emit_call(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
    action: &GrammarAction,
) -> Result<FrontendExpression, GrammarActionError> {
    let callee = action_text_arg(node, action, "callee")?;
    let args = action_expression_list_arg(rules, node, action, "args").unwrap_or_default();
    Ok(FrontendExpression::Call {
        callee: callee.value,
        args,
        source_span: node.span.into(),
    })
}

fn evaluate_emit_argument_list(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
    action: &GrammarAction,
) -> Result<Vec<FrontendExpression>, GrammarActionError> {
    let mut args = Vec::from([action_expression_arg(rules, node, action, "first")?]);
    args.extend(action_expression_list_arg(rules, node, action, "rest").unwrap_or_default());
    Ok(args)
}

fn evaluate_fold_binary(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
    action: &GrammarAction,
) -> Result<FrontendExpression, GrammarActionError> {
    let mut lhs = action_expression_arg(rules, node, action, "lhs")?;
    for tail in action_binary_tails_arg(rules, node, action, "tails")? {
        let source_span = Span::new(expr_span(&lhs).start, expr_span(&tail.rhs).end);
        lhs = FrontendExpression::Binary {
            op: tail.op,
            lhs: alloc::boxed::Box::new(lhs),
            rhs: alloc::boxed::Box::new(tail.rhs),
            source_span,
        };
    }
    Ok(lhs)
}

fn action_text_arg(
    node: &InterpretedRuleNode,
    action: &GrammarAction,
    name: &str,
) -> Result<TextValue, GrammarActionError> {
    let Some(arg) = action.args.iter().find(|arg| arg.name == name) else {
        return Err(GrammarActionError::new(
            node.span,
            format!("grammar action argument `{name}`"),
        )
        .with_arg(name));
    };
    match &arg.value {
        GrammarActionTerm::Capture { rule } => node
            .children
            .iter()
            .find(|child| child.rule_name == *rule)
            .map(|child| TextValue {
                value: child.text.trim().to_string(),
                span: child.span.into(),
            })
            .ok_or_else(|| {
                GrammarActionError::new(node.span, format!("capture `{rule}`")).with_arg(name)
            }),
        GrammarActionTerm::Literal(value) => Ok(TextValue {
            value: value.clone(),
            span: node.span.into(),
        }),
        _ => Err(
            GrammarActionError::new(node.span, format!("text argument `{name}`")).with_arg(name),
        ),
    }
}

fn action_expression_arg(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
    action: &GrammarAction,
    name: &str,
) -> Result<FrontendExpression, GrammarActionError> {
    let value = action_capture_value(rules, node, action, name)?;
    match value {
        GrammarActionValue::FrontendExpression(expression) => Ok(expression),
        _ => Err(
            GrammarActionError::new(node.span, format!("expression argument `{name}`"))
                .with_arg(name),
        ),
    }
}

fn action_expression_list_arg(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
    action: &GrammarAction,
    name: &str,
) -> Result<Vec<FrontendExpression>, GrammarActionError> {
    let values = action_list_arg(rules, node, action, name)?;
    let mut expressions = Vec::new();
    for value in values {
        match value {
            GrammarActionValue::FrontendExpression(expression) => expressions.push(expression),
            GrammarActionValue::FrontendExpressionList(items) => expressions.extend(items),
            _ => {
                return Err(GrammarActionError::new(
                    node.span,
                    format!("expression list argument `{name}`"),
                )
                .with_arg(name));
            }
        }
    }
    Ok(expressions)
}

fn action_binary_tails_arg(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
    action: &GrammarAction,
    name: &str,
) -> Result<Vec<BinaryTail>, GrammarActionError> {
    action_list_arg(rules, node, action, name)?
        .into_iter()
        .map(|value| match value {
            GrammarActionValue::BinaryTail { op, rhs, span } => Ok(BinaryTail { op, rhs, span }),
            _ => Err(
                GrammarActionError::new(node.span, format!("binary tail argument `{name}`"))
                    .with_arg(name),
            ),
        })
        .collect()
}

fn action_capture_value(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
    action: &GrammarAction,
    name: &str,
) -> Result<GrammarActionValue, GrammarActionError> {
    let Some(arg) = action.args.iter().find(|arg| arg.name == name) else {
        return Err(GrammarActionError::new(
            node.span,
            format!("grammar action argument `{name}`"),
        )
        .with_arg(name));
    };
    let GrammarActionTerm::Capture { rule } = &arg.value else {
        return Err(
            GrammarActionError::new(node.span, format!("capture argument `{name}`")).with_arg(name),
        );
    };
    let child = node
        .children
        .iter()
        .find(|child| child.rule_name == *rule)
        .ok_or_else(|| {
            GrammarActionError::new(node.span, format!("capture `{rule}`")).with_arg(name)
        })?;
    evaluate_node_value(rules, child)
}

fn evaluate_node_value(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
) -> Result<GrammarActionValue, GrammarActionError> {
    if let Some(output) = evaluate_rule_action(rules, node)? {
        return Ok(output.value);
    }
    if let Some(value) =
        node.children
            .iter()
            .find_map(|child| match evaluate_node_value(rules, child) {
                Ok(GrammarActionValue::FrontendExpression(expression)) => {
                    Some(Ok(GrammarActionValue::FrontendExpression(expression)))
                }
                Ok(GrammarActionValue::FrontendExpressionList(items)) => {
                    Some(Ok(GrammarActionValue::FrontendExpressionList(items)))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
    {
        return value;
    }
    Ok(GrammarActionValue::Text {
        value: node.text.trim().to_string(),
        span: node.span.into(),
    })
}

fn action_bool_arg(action: &GrammarAction, name: &str) -> Option<bool> {
    action
        .args
        .iter()
        .find(|arg| arg.name == name)
        .and_then(|arg| match arg.value {
            GrammarActionTerm::Bool(value) => Some(value),
            _ => None,
        })
}

fn action_list_arg(
    rules: &[SourceAuthoredRule],
    node: &InterpretedRuleNode,
    action: &GrammarAction,
    name: &str,
) -> Result<Vec<GrammarActionValue>, GrammarActionError> {
    let Some(arg) = action.args.iter().find(|arg| arg.name == name) else {
        return Err(GrammarActionError::new(
            node.span,
            format!("grammar action argument `{name}`"),
        )
        .with_arg(name));
    };
    let GrammarActionTerm::CaptureAll { rule } = &arg.value else {
        return Err(
            GrammarActionError::new(node.span, format!("list argument `{name}`")).with_arg(name),
        );
    };
    node.children
        .iter()
        .filter(|child| child.rule_name == *rule)
        .map(|child| {
            evaluate_rule_action(rules, child)?.map_or_else(
                || {
                    Ok(GrammarActionValue::Text {
                        value: child.text.trim().to_string(),
                        span: child.span.into(),
                    })
                },
                |output| Ok(output.value),
            )
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BinaryTail {
    op: FrontendBinaryOp,
    rhs: FrontendExpression,
    span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TextValue {
    value: String,
    span: Span,
}

fn binary_op_from_text(text: &str) -> Option<FrontendBinaryOp> {
    match text.trim() {
        "+" => Some(FrontendBinaryOp::Add),
        "-" => Some(FrontendBinaryOp::Sub),
        "*" => Some(FrontendBinaryOp::Mul),
        _ => None,
    }
}

fn expr_span(expression: &FrontendExpression) -> Span {
    match expression {
        FrontendExpression::Integer { source_span, .. }
        | FrontendExpression::Name { source_span, .. }
        | FrontendExpression::Call { source_span, .. }
        | FrontendExpression::Binary { source_span, .. } => *source_span,
    }
}

fn class_name_from_action_text(text: &str) -> Option<String> {
    let text = text.trim();
    if text.starts_with('<') {
        valid_type_name(text).then(|| text.to_string())
    } else {
        valid_record_type_fragment(text).then(|| format!("<{text}>"))
    }
}

fn grammar_action_output_source_data(
    action: &GrammarAction,
    value: &GrammarActionValue,
) -> SourceData {
    SourceData::record(
        "grammar-action-output",
        vec![
            source_data::field("action", grammar_action_source_data(action)),
            source_data::field("value", grammar_action_value_source_data(value)),
        ],
    )
}

fn grammar_action_value_source_data(value: &GrammarActionValue) -> SourceData {
    match value {
        GrammarActionValue::Text { value, span } => SourceData::record(
            "action-text",
            vec![
                source_data::field("value", SourceData::string(value.as_str())),
                source_span_data(*span),
            ],
        ),
        GrammarActionValue::FrontendSlot(slot) => frontend_slot_source_data(slot),
        GrammarActionValue::FrontendForm(form) => frontend_form_source_data(form),
        GrammarActionValue::FrontendExpression(expression) => {
            frontend_expression_source_data(expression)
        }
        GrammarActionValue::FrontendExpressionList(items) => {
            SourceData::list(items.iter().map(frontend_expression_source_data).collect())
        }
        GrammarActionValue::BinaryTail { op, rhs, span } => SourceData::record(
            "binary-tail",
            vec![
                source_data::field("op", SourceData::symbol(op.as_str())),
                source_data::field("rhs", frontend_expression_source_data(rhs)),
                source_span_data(*span),
            ],
        ),
    }
}

pub(super) fn interpreted_rule_node_source_data(node: &InterpretedRuleNode) -> SourceData {
    SourceData::record(
        "rule-node",
        vec![
            source_data::field("rule", SourceData::symbol(node.rule_name.as_str())),
            source_span_data(node.span),
            source_data::field(
                "children",
                SourceData::list(
                    node.children
                        .iter()
                        .map(interpreted_rule_node_source_data)
                        .collect(),
                ),
            ),
        ],
    )
}

pub(super) fn interpreted_rule_counts(root: &InterpretedRuleNode) -> Vec<StagedRuleCount> {
    let mut counts = Vec::<StagedRuleCount>::new();
    count_interpreted_rule(root, &mut counts);
    counts
}

fn count_interpreted_rule(node: &InterpretedRuleNode, counts: &mut Vec<StagedRuleCount>) {
    if let Some(count) = counts
        .iter_mut()
        .find(|count| count.rule_name == node.rule_name)
    {
        count.node_count = count.node_count.saturating_add(1);
    } else {
        counts.push(StagedRuleCount {
            rule_name: node.rule_name.clone(),
            node_count: 1,
        });
    }
    for child in &node.children {
        count_interpreted_rule(child, counts);
    }
}
