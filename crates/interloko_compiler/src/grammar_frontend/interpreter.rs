use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::source_data::{self, SourceData};
use crate::syntax::Span;

use super::{
    FrontendBinaryOp, FrontendExpression, FrontendForm, FrontendSlot, GrammarAction,
    GrammarActionTerm, RuleExpression, StagedRuleCount, frontend_expression_source_data,
    frontend_form_source_data, frontend_slot_source_data, parse_grammar_action,
    projected_rule_expression, source_span_data, valid_calculator_function_name,
    valid_record_field_name, valid_record_type_fragment, valid_type_name,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceAuthoredRule {
    pub(super) name: String,
    pub(super) expression: RuleExpression,
    pub(super) action: Option<GrammarAction>,
    pub(super) source_span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct InterpretedRuleNode {
    pub(super) rule_name: String,
    pub(super) span: Span,
    pub(super) text: String,
    pub(super) children: Vec<InterpretedRuleNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct InterpretedParse {
    pub(super) root: InterpretedRuleNode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct InterpretedParseError {
    pub(super) span: Span,
    pub(super) expected: Vec<String>,
}

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
    pub(super) expected: String,
    pub(super) field: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpressionMatch {
    end: usize,
    children: Vec<InterpretedRuleNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParseFailure {
    position: usize,
    expected: Vec<String>,
}

pub(super) fn parse_source_authored_grammar(source: &str) -> Vec<SourceAuthoredRule> {
    let mut rules = Vec::new();
    let mut offset = 0_usize;
    for line in source.split_inclusive('\n') {
        let line_start = offset;
        let line_end = offset + line.len();
        offset = line_end;
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("grammar ")
            || trimmed == "end"
        {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("rule ") else {
            continue;
        };
        let Some((name, expression)) = rest.split_once('=') else {
            continue;
        };
        let action = expression
            .split_once("=>")
            .and_then(|(_, output)| parse_grammar_action(output.trim()));
        let leading = line.find(trimmed).unwrap_or(0);
        let trimmed_start = line_start + leading;
        rules.push(SourceAuthoredRule {
            name: name.trim().to_string(),
            expression: projected_rule_expression(expression),
            action,
            source_span: Span::new(trimmed_start, line_end.saturating_sub(1)),
        });
    }
    rules
}

pub(super) fn interpret_region_result(
    rules: &[SourceAuthoredRule],
    root_rule: &str,
    source: &str,
    source_offset: usize,
) -> Result<InterpretedParse, InterpretedParseError> {
    let root = interpret_rule_result(rules, root_rule, source, 0, source_offset)
        .map_err(|failure| failure.into_error(source_offset))?;
    let trailing = skip_interpreter_trivia(source, root.span.end.saturating_sub(source_offset));
    if trailing == source.len() {
        Ok(InterpretedParse { root })
    } else {
        Err(ParseFailure::expected(trailing, "end of region").into_error(source_offset))
    }
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
            GrammarActionValue::FrontendForm(evaluate_emit_class(rules, node, &action)?)
        }
        "emit-slot" => GrammarActionValue::FrontendSlot(evaluate_emit_slot(node, &action)?),
        "emit-function" => {
            GrammarActionValue::FrontendForm(evaluate_emit_function(rules, node, &action)?)
        }
        "emit-integer" => {
            GrammarActionValue::FrontendExpression(evaluate_emit_integer(node, &action)?)
        }
        "emit-name" => GrammarActionValue::FrontendExpression(evaluate_emit_name(node, &action)?),
        "emit-call" => {
            GrammarActionValue::FrontendExpression(evaluate_emit_call(rules, node, &action)?)
        }
        "emit-argument-list" => GrammarActionValue::FrontendExpressionList(
            evaluate_emit_argument_list(rules, node, &action)?,
        ),
        "emit-argument-tail" => {
            GrammarActionValue::FrontendExpressionList(Vec::from([action_expression_arg(
                rules, node, &action, "value",
            )?]))
        }
        "emit-binary-tail" => {
            let op = action_text_arg(node, &action, "op")?;
            let rhs = action_expression_arg(rules, node, &action, "rhs")?;
            GrammarActionValue::BinaryTail {
                op: binary_op_from_text(&op.value).ok_or_else(|| GrammarActionError {
                    span: op.span,
                    expected: "binary operator".to_string(),
                    field: None,
                })?,
                span: Span::new(op.span.start, expr_span(&rhs).end),
                rhs,
            }
        }
        "fold-binary" => {
            GrammarActionValue::FrontendExpression(evaluate_fold_binary(rules, node, &action)?)
        }
        _ => {
            return Err(GrammarActionError {
                span: node.span,
                expected: format!("known grammar action `{}`", action.name),
                field: None,
            });
        }
    };
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
    let class_name =
        class_name_from_action_text(&name.value).ok_or_else(|| GrammarActionError {
            span: name.span,
            expected: "valid class name".to_string(),
            field: None,
        })?;
    let superclass = action_text_arg(node, action, "superclass")
        .map(|value| value.value)
        .unwrap_or_else(|_| "<object>".to_string());
    let slots = action_list_arg(rules, node, action, "slots")?
        .into_iter()
        .map(|value| match value {
            GrammarActionValue::FrontendSlot(slot) => Ok(slot),
            _ => Err(GrammarActionError {
                span: node.span,
                expected: "slot action output".to_string(),
                field: None,
            }),
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (index, slot) in slots.iter().enumerate() {
        if slots[..index].iter().any(|prior| prior.name == slot.name) {
            return Err(GrammarActionError {
                span: slot.source_span,
                expected: "unique record field".to_string(),
                field: Some(slot.name.clone()),
            });
        }
    }
    Ok(FrontendForm::class(
        class_name, superclass, slots, node.span,
    ))
}

fn evaluate_emit_slot(
    node: &InterpretedRuleNode,
    action: &GrammarAction,
) -> Result<FrontendSlot, GrammarActionError> {
    let name = action_text_arg(node, action, "name")?;
    let type_name = action_text_arg(node, action, "type")?;
    if !valid_record_field_name(name.value.trim()) {
        return Err(GrammarActionError {
            span: name.span,
            expected: "valid record field".to_string(),
            field: Some(name.value.clone()),
        });
    }
    if !valid_type_name(type_name.value.trim()) {
        return Err(GrammarActionError {
            span: type_name.span,
            expected: "field type name".to_string(),
            field: Some(name.value.clone()),
        });
    }
    let init_keyword = action_text_arg(node, action, "init-keyword")
        .map(|value| value.value)
        .ok();
    let required = action_bool_arg(action, "required-init-keyword").unwrap_or(false);
    let mut slot = FrontendSlot::new(name.value.trim(), type_name.value.trim(), node.span);
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
        return Err(GrammarActionError {
            span: name.span,
            expected: "`name = expression`".to_string(),
            field: None,
        });
    }
    let body = action_expression_arg(rules, node, action, "body")?;
    Ok(FrontendForm::function(name.value.trim(), body, node.span))
}

fn evaluate_emit_integer(
    node: &InterpretedRuleNode,
    action: &GrammarAction,
) -> Result<FrontendExpression, GrammarActionError> {
    let value = action_text_arg(node, action, "value")?;
    Ok(FrontendExpression::Integer {
        value: value.value.parse().map_err(|_| GrammarActionError {
            span: value.span,
            expected: "integer literal".to_string(),
            field: None,
        })?,
        source_span: node.span,
    })
}

fn evaluate_emit_name(
    node: &InterpretedRuleNode,
    action: &GrammarAction,
) -> Result<FrontendExpression, GrammarActionError> {
    let name = action_text_arg(node, action, "name")?;
    Ok(FrontendExpression::Name {
        name: name.value,
        source_span: node.span,
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
        source_span: node.span,
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
        return Err(GrammarActionError {
            span: node.span,
            expected: format!("grammar action argument `{name}`"),
            field: None,
        });
    };
    match &arg.value {
        GrammarActionTerm::Capture { rule } => node
            .children
            .iter()
            .find(|child| child.rule_name == *rule)
            .map(|child| TextValue {
                value: child.text.trim().to_string(),
                span: child.span,
            })
            .ok_or_else(|| GrammarActionError {
                span: node.span,
                expected: format!("capture `{rule}`"),
                field: None,
            }),
        GrammarActionTerm::Literal(value) => Ok(TextValue {
            value: value.clone(),
            span: node.span,
        }),
        _ => Err(GrammarActionError {
            span: node.span,
            expected: format!("text argument `{name}`"),
            field: None,
        }),
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
        _ => Err(GrammarActionError {
            span: node.span,
            expected: format!("expression argument `{name}`"),
            field: None,
        }),
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
                return Err(GrammarActionError {
                    span: node.span,
                    expected: format!("expression list argument `{name}`"),
                    field: None,
                });
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
            _ => Err(GrammarActionError {
                span: node.span,
                expected: format!("binary tail argument `{name}`"),
                field: None,
            }),
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
        return Err(GrammarActionError {
            span: node.span,
            expected: format!("grammar action argument `{name}`"),
            field: None,
        });
    };
    let GrammarActionTerm::Capture { rule } = &arg.value else {
        return Err(GrammarActionError {
            span: node.span,
            expected: format!("capture argument `{name}`"),
            field: None,
        });
    };
    let child = node
        .children
        .iter()
        .find(|child| child.rule_name == *rule)
        .ok_or_else(|| GrammarActionError {
            span: node.span,
            expected: format!("capture `{rule}`"),
            field: None,
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
        span: node.span,
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
        return Err(GrammarActionError {
            span: node.span,
            expected: format!("grammar action argument `{name}`"),
            field: None,
        });
    };
    let GrammarActionTerm::CaptureAll { rule } = &arg.value else {
        return Err(GrammarActionError {
            span: node.span,
            expected: format!("list argument `{name}`"),
            field: None,
        });
    };
    node.children
        .iter()
        .filter(|child| child.rule_name == *rule)
        .map(|child| {
            evaluate_rule_action(rules, child)?.map_or_else(
                || {
                    Ok(GrammarActionValue::Text {
                        value: child.text.trim().to_string(),
                        span: child.span,
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
            source_data::field("action", action.source_data()),
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

fn interpret_rule_result(
    rules: &[SourceAuthoredRule],
    name: &str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> Result<InterpretedRuleNode, ParseFailure> {
    if let Some(rule) = rules.iter().find(|rule| rule.name == name) {
        let start = skip_interpreter_trivia(source, position);
        let matched = interpret_expression(rules, &rule.expression, source, start, source_offset)?;
        let end = matched.end;
        return Ok(InterpretedRuleNode {
            rule_name: name.to_string(),
            span: Span::new(source_offset + start, source_offset + end),
            text: source[start..end].to_string(),
            children: matched.children,
        });
    }
    interpret_builtin_rule(name, source, position, source_offset)
        .ok_or_else(|| ParseFailure::expected(skip_interpreter_trivia(source, position), name))
}

fn interpret_expression(
    rules: &[SourceAuthoredRule],
    expression: &RuleExpression,
    source: &str,
    position: usize,
    source_offset: usize,
) -> Result<ExpressionMatch, ParseFailure> {
    match expression {
        RuleExpression::Rule(name) => {
            let node = interpret_rule_result(rules, name, source, position, source_offset)?;
            Ok(ExpressionMatch {
                end: node.span.end.saturating_sub(source_offset),
                children: vec![node],
            })
        }
        RuleExpression::Literal(value) => {
            let start = skip_interpreter_trivia(source, position);
            let end = source[start..]
                .starts_with(value)
                .then_some(start + value.len())
                .filter(|end| literal_boundary_ok(source, *end, value))
                .ok_or_else(|| ParseFailure::expected(start, literal_expected(value)))?;
            Ok(ExpressionMatch {
                end,
                children: vec![InterpretedRuleNode {
                    rule_name: format!("literal:{value}"),
                    span: Span::new(source_offset + start, source_offset + end),
                    text: source[start..end].to_string(),
                    children: Vec::new(),
                }],
            })
        }
        RuleExpression::CharacterClass(value) => {
            let start = position;
            let ch = source[start..]
                .chars()
                .next()
                .filter(|ch| character_class_contains(value, *ch))
                .ok_or_else(|| ParseFailure::expected(start, character_class_expected(value)))?;
            let end = start + ch.len_utf8();
            Ok(ExpressionMatch {
                end,
                children: vec![InterpretedRuleNode {
                    rule_name: format!("class:{value}"),
                    span: Span::new(source_offset + start, source_offset + end),
                    text: source[start..end].to_string(),
                    children: Vec::new(),
                }],
            })
        }
        RuleExpression::Sequence(items) => {
            let mut end = position;
            let mut children = Vec::new();
            for item in items {
                let matched = interpret_expression(rules, item, source, end, source_offset)?;
                end = matched.end;
                children.extend(matched.children);
            }
            Ok(ExpressionMatch { end, children })
        }
        RuleExpression::Choice(items) => {
            let mut failure = None;
            for item in items {
                match interpret_expression(rules, item, source, position, source_offset) {
                    Ok(matched) => return Ok(matched),
                    Err(error) => merge_failure(&mut failure, error),
                }
            }
            Err(failure.unwrap_or_else(|| ParseFailure::expected(position, "choice")))
        }
        RuleExpression::Optional(item) => {
            Ok(
                interpret_expression(rules, item, source, position, source_offset).unwrap_or_else(
                    |_| ExpressionMatch {
                        end: position,
                        children: Vec::new(),
                    },
                ),
            )
        }
        RuleExpression::OneOrMore(item) => {
            let first = interpret_expression(rules, item, source, position, source_offset)?;
            let mut end = first.end;
            let mut children = first.children;
            while let Ok(matched) = interpret_expression(rules, item, source, end, source_offset) {
                if matched.end == end {
                    break;
                }
                end = matched.end;
                children.extend(matched.children);
            }
            Ok(ExpressionMatch { end, children })
        }
        RuleExpression::ZeroOrMore(item) => {
            let mut end = position;
            let mut children = Vec::new();
            while let Ok(matched) = interpret_expression(rules, item, source, end, source_offset) {
                if matched.end == end {
                    break;
                }
                end = matched.end;
                children.extend(matched.children);
            }
            Ok(ExpressionMatch { end, children })
        }
        RuleExpression::Transform { input, .. } => {
            interpret_expression(rules, input, source, position, source_offset)
        }
    }
}

impl ParseFailure {
    fn expected(position: usize, expected: impl Into<String>) -> Self {
        Self {
            position,
            expected: vec![expected.into()],
        }
    }

    fn into_error(self, source_offset: usize) -> InterpretedParseError {
        InterpretedParseError {
            span: Span::new(source_offset + self.position, source_offset + self.position),
            expected: self.expected,
        }
    }
}

fn merge_failure(failure: &mut Option<ParseFailure>, next: ParseFailure) {
    match failure {
        Some(current) if current.position > next.position => {}
        Some(current) if current.position == next.position => {
            for expected in next.expected {
                if !current.expected.contains(&expected) {
                    current.expected.push(expected);
                }
            }
        }
        _ => *failure = Some(next),
    }
}

fn literal_expected(value: &str) -> String {
    format!("`{value}`")
}

fn character_class_expected(value: &str) -> String {
    format!("[{value}]")
}

fn character_class_contains(class: &str, ch: char) -> bool {
    let mut chars = class.chars().peekable();
    let mut previous = None;
    while let Some(current) = chars.next() {
        if current == '-'
            && let (Some(start), Some(&end)) = (previous, chars.peek())
        {
            let _ = chars.next();
            if start <= ch && ch <= end {
                return true;
            }
            previous = Some(end);
            continue;
        }
        if current == ch {
            return true;
        }
        previous = Some(current);
    }
    false
}

fn interpret_builtin_rule(
    name: &str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> Option<InterpretedRuleNode> {
    let start = skip_interpreter_trivia(source, position);
    // Keep these builtins intentionally tiny. Calculator still uses them while
    // staged grammars gain enough lexical PEG surface to define tokens directly.
    let end = match name {
        "name" => match source.as_bytes().get(start).copied() {
            Some(b'<') => parse_angle_name(source, start)?,
            _ => parse_identifier_like(source, start)?,
        },
        "type-name" => parse_angle_name(source, start)?,
        "number" => parse_number(source, start)?,
        _ => return None,
    };
    Some(InterpretedRuleNode {
        rule_name: name.to_string(),
        span: Span::new(source_offset + start, source_offset + end),
        text: source[start..end].to_string(),
        children: Vec::new(),
    })
}

fn skip_interpreter_trivia(source: &str, mut position: usize) -> usize {
    loop {
        let rest = &source[position..];
        let skipped = rest
            .char_indices()
            .take_while(|(_, ch)| ch.is_whitespace())
            .last()
            .map_or(0, |(index, ch)| index + ch.len_utf8());
        position += skipped;
        if source[position..].starts_with("//") {
            if let Some(newline) = source[position..].find('\n') {
                position += newline + 1;
                continue;
            }
            return source.len();
        }
        return position;
    }
}

fn literal_boundary_ok(source: &str, end: usize, literal: &str) -> bool {
    if !literal
        .bytes()
        .last()
        .is_some_and(|byte| byte.is_ascii_alphabetic())
    {
        return true;
    }
    source
        .as_bytes()
        .get(end)
        .is_none_or(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-' | b'?'))
}

fn parse_identifier_like(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let first = *bytes.get(start)?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let mut end = start + 1;
    while bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'?'))
    {
        end += 1;
    }
    Some(end)
}

fn parse_angle_name(source: &str, start: usize) -> Option<usize> {
    let rest = source.get(start..)?;
    if !rest.starts_with('<') {
        return None;
    }
    rest.find('>').map(|index| start + index + 1)
}

fn parse_number(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if !bytes.get(start).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let mut end = start + 1;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    Some(end)
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
