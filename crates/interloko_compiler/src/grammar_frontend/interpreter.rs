use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::source_data::{self, SourceData};
use crate::syntax::Span;

use super::{RuleExpression, StagedRuleCount, projected_rule_expression, source_span_data};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceAuthoredRule {
    pub(super) name: String,
    pub(super) expression: RuleExpression,
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
        let leading = line.find(trimmed).unwrap_or(0);
        let trimmed_start = line_start + leading;
        rules.push(SourceAuthoredRule {
            name: name.trim().to_string(),
            expression: projected_rule_expression(expression),
            source_span: Span::new(trimmed_start, line_end.saturating_sub(1)),
        });
    }
    rules
}

pub(super) fn interpret_region(
    rules: &[SourceAuthoredRule],
    root_rule: &str,
    source: &str,
    source_offset: usize,
) -> Option<InterpretedParse> {
    interpret_region_result(rules, root_rule, source, source_offset).ok()
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

pub(super) fn interpret_rule(
    rules: &[SourceAuthoredRule],
    name: &str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> Option<InterpretedRuleNode> {
    interpret_rule_result(rules, name, source, position, source_offset).ok()
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

fn interpret_builtin_rule(
    name: &str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> Option<InterpretedRuleNode> {
    let start = skip_interpreter_trivia(source, position);
    // Keep these builtins intentionally tiny. They stand in for character-class
    // rules until source-authored grammars can express identifiers, type names,
    // and numbers structurally.
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
