use alloc::boxed::Box;
use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use crate::diagnostic::Diagnostic;
use crate::source_data::{self, SourceData};
use crate::syntax::Span;

use super::interpreter::{
    InterpretedParse, InterpretedRuleNode, interpret_region, interpreted_rule_counts,
    interpreted_rule_node_source_data, parse_source_authored_grammar,
};
use super::{
    CALCULATOR_GRAMMAR_NAME, FrontendBinaryOp, FrontendExpression, FrontendForm, GrammarId,
    GrammarProjection, GrammarRule, GrammarRuleId, ProjectionOrigin, StagedRegionEmission,
    grammar_rule_expression_data_from_expr, source_span_data, staged_expected_diagnostic,
    usize_to_source_i64, valid_calculator_function_name,
};

const CALCULATOR_GRAMMAR_SOURCE: &str = "\
grammar calculator\n\
rule calculator-region = calculator-function*\n\
rule calculator-function = name `=` expression => frontend-function\n\
rule expression = term additive-tail*\n\
rule additive-tail = add-op term\n\
rule add-op = `+` | `-`\n\
rule term = factor multiplicative-tail*\n\
rule multiplicative-tail = `*` factor\n\
rule factor = parenthesized | call | number | name\n\
rule parenthesized = `(` expression `)`\n\
rule call = name `(` argument-list? `)`\n\
rule argument-list = expression argument-tail*\n\
rule argument-tail = `,` expression\n\
end\n";

pub(super) fn parse_region(body: &str, source_offset: usize) -> StagedRegionEmission {
    if let Some(parse) = interpret_calculator_region(body, source_offset) {
        return emission_from_interpreted_parse(body, source_offset, &parse)
            .unwrap_or_else(|| parse_region_legacy(body, source_offset));
    }
    parse_region_legacy(body, source_offset)
}

fn interpret_calculator_region(body: &str, source_offset: usize) -> Option<InterpretedParse> {
    let rules = parse_source_authored_grammar(CALCULATOR_GRAMMAR_SOURCE);
    interpret_region(&rules, "calculator-region", body, source_offset)
}

fn emission_from_interpreted_parse(
    body: &str,
    source_offset: usize,
    parse: &InterpretedParse,
) -> Option<StagedRegionEmission> {
    let mut forms = Vec::new();
    let mut diagnostics = Vec::new();
    let mut functions = Vec::new();
    for function in parse
        .root
        .children
        .iter()
        .filter(|node| node.rule_name == "calculator-function")
    {
        let Some((form, data)) =
            frontend_function_from_interpreted_node(function, &mut diagnostics)
        else {
            continue;
        };
        forms.push(form);
        functions.push(data);
    }
    if functions.is_empty() && diagnostics.is_empty() {
        diagnostics.push(staged_expected_diagnostic(
            Span::new(source_offset, source_offset + body.len()),
            "calculator",
            "`name = expression`",
        ));
    }
    Some(StagedRegionEmission {
        forms,
        data: SourceData::record(
            "calculator-region",
            vec![
                source_data::field("grammar", SourceData::symbol(CALCULATOR_GRAMMAR_NAME)),
                source_data::field("driver", SourceData::symbol("interpreted-grammar")),
                source_data::field("parse-tree", interpreted_rule_node_source_data(&parse.root)),
                source_data::field("functions", SourceData::list(functions)),
            ],
        ),
        rule_counts: interpreted_rule_counts(&parse.root),
        diagnostics,
    })
}

fn frontend_function_from_interpreted_node(
    function: &InterpretedRuleNode,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(FrontendForm, SourceData)> {
    let name = function
        .children
        .iter()
        .find(|node| node.rule_name == "name")?;
    if !valid_calculator_function_name(name.text.trim()) {
        diagnostics.push(staged_expected_diagnostic(
            name.span,
            "calculator",
            "`name = expression`",
        ));
        return None;
    }
    let expression = function
        .children
        .iter()
        .find(|node| node.rule_name == "expression")?;
    let Some(expression_form) = frontend_expression_from_node(expression) else {
        diagnostics.push(staged_expected_diagnostic(
            expression.span,
            "calculator",
            "calculator expression",
        ));
        return None;
    };
    Some((
        FrontendForm::function(name.text.trim(), expression_form, function.span),
        SourceData::record(
            "calculator-function",
            vec![
                source_data::field("name", SourceData::symbol(name.text.trim())),
                source_data::field("expression", SourceData::string(expression.text.trim())),
                source_span_data(function.span),
            ],
        ),
    ))
}

fn frontend_expression_from_node(node: &InterpretedRuleNode) -> Option<FrontendExpression> {
    match node.rule_name.as_str() {
        "expression" => expression_node(node),
        "term" => term_node(node),
        "factor" => node.children.iter().find_map(frontend_expression_from_node),
        "parenthesized" => node
            .children
            .iter()
            .find(|child| child.rule_name == "expression")
            .and_then(frontend_expression_from_node),
        "number" => integer_node(node),
        "name" => Some(FrontendExpression::Name {
            name: node.text.trim().to_string(),
            source_span: node.span,
        }),
        "call" => call_node(node),
        _ => None,
    }
}

fn expression_node(node: &InterpretedRuleNode) -> Option<FrontendExpression> {
    let mut lhs = node
        .children
        .iter()
        .find(|child| child.rule_name == "term")
        .and_then(frontend_expression_from_node)?;
    for tail in node
        .children
        .iter()
        .filter(|child| child.rule_name == "additive-tail")
    {
        let op = match tail
            .children
            .iter()
            .find(|child| child.rule_name == "add-op")?
        {
            add if add
                .children
                .iter()
                .any(|child| child.rule_name == "literal:+") =>
            {
                FrontendBinaryOp::Add
            }
            add if add
                .children
                .iter()
                .any(|child| child.rule_name == "literal:-") =>
            {
                FrontendBinaryOp::Sub
            }
            _ => return None,
        };
        let rhs = tail
            .children
            .iter()
            .find(|child| child.rule_name == "term")
            .and_then(frontend_expression_from_node)?;
        let source_span = Span::new(expr_span(&lhs).start, expr_span(&rhs).end);
        lhs = FrontendExpression::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            source_span,
        };
    }
    Some(lhs)
}

fn term_node(node: &InterpretedRuleNode) -> Option<FrontendExpression> {
    let mut lhs = node
        .children
        .iter()
        .find(|child| child.rule_name == "factor")
        .and_then(frontend_expression_from_node)?;
    for tail in node
        .children
        .iter()
        .filter(|child| child.rule_name == "multiplicative-tail")
    {
        if !tail
            .children
            .iter()
            .any(|child| child.rule_name == "literal:*")
        {
            return None;
        }
        let rhs = tail
            .children
            .iter()
            .find(|child| child.rule_name == "factor")
            .and_then(frontend_expression_from_node)?;
        let source_span = Span::new(expr_span(&lhs).start, expr_span(&rhs).end);
        lhs = FrontendExpression::Binary {
            op: FrontendBinaryOp::Mul,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            source_span,
        };
    }
    Some(lhs)
}

fn integer_node(node: &InterpretedRuleNode) -> Option<FrontendExpression> {
    Some(FrontendExpression::Integer {
        value: node.text.trim().parse().ok()?,
        source_span: node.span,
    })
}

fn call_node(node: &InterpretedRuleNode) -> Option<FrontendExpression> {
    let name = node
        .children
        .iter()
        .find(|child| child.rule_name == "name")?;
    let args = node
        .children
        .iter()
        .find(|child| child.rule_name == "argument-list")
        .map_or_else(|| Some(Vec::new()), argument_list_node)?;
    Some(FrontendExpression::Call {
        callee: name.text.trim().to_string(),
        args,
        source_span: node.span,
    })
}

fn argument_list_node(node: &InterpretedRuleNode) -> Option<Vec<FrontendExpression>> {
    let mut args = Vec::new();
    let first = node
        .children
        .iter()
        .find(|child| child.rule_name == "expression")
        .and_then(frontend_expression_from_node)?;
    args.push(first);
    for tail in node
        .children
        .iter()
        .filter(|child| child.rule_name == "argument-tail")
    {
        let arg = tail
            .children
            .iter()
            .find(|child| child.rule_name == "expression")
            .and_then(frontend_expression_from_node)?;
        args.push(arg);
    }
    Some(args)
}

fn parse_region_legacy(body: &str, source_offset: usize) -> StagedRegionEmission {
    let mut forms = Vec::new();
    let mut diagnostics = Vec::new();
    let mut functions = Vec::new();
    let mut offset = source_offset;

    for line in body.split_inclusive('\n') {
        let line_start = offset;
        let line_end = offset + line.len();
        offset = line_end;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let leading = line.find(trimmed).unwrap_or(0);
        let trimmed_start = line_start + leading;
        let trimmed_span = Span::new(trimmed_start, trimmed_start + trimmed.len());
        let Some((name, expression)) = trimmed.split_once('=') else {
            diagnostics.push(staged_expected_diagnostic(
                trimmed_span,
                "calculator",
                "`name = expression`",
            ));
            continue;
        };
        let name = name.trim();
        let expression = expression.trim();
        if name.is_empty() || expression.is_empty() || !valid_calculator_function_name(name) {
            diagnostics.push(staged_expected_diagnostic(
                trimmed_span,
                "calculator",
                "`name = expression`",
            ));
            continue;
        }
        let expression_offset = trimmed.find(expression).unwrap_or(0);
        let expression_start = trimmed_start + expression_offset;
        let expression_span = Span::new(expression_start, expression_start + expression.len());
        let Some(expression_form) = parse_calculator_expression(expression, expression_start)
        else {
            diagnostics.push(staged_expected_diagnostic(
                expression_span,
                "calculator",
                "calculator expression",
            ));
            continue;
        };
        forms.push(FrontendForm::function(name, expression_form, trimmed_span));
        functions.push(SourceData::record(
            "calculator-function",
            vec![
                source_data::field("name", SourceData::symbol(name)),
                source_data::field("expression", SourceData::string(expression)),
                source_data::field(
                    "source-start",
                    SourceData::integer(usize_to_source_i64(trimmed_span.start)),
                ),
                source_data::field(
                    "source-end",
                    SourceData::integer(usize_to_source_i64(trimmed_span.end)),
                ),
            ],
        ));
    }

    if functions.is_empty() && diagnostics.is_empty() {
        diagnostics.push(staged_expected_diagnostic(
            Span::new(source_offset, source_offset + body.len()),
            "calculator",
            "`name = expression`",
        ));
    }

    let function_count = forms.len();
    StagedRegionEmission {
        forms,
        data: SourceData::record(
            "calculator-region",
            vec![
                source_data::field("grammar", SourceData::symbol(CALCULATOR_GRAMMAR_NAME)),
                source_data::field("functions", SourceData::list(functions)),
            ],
        ),
        rule_counts: Vec::from([
            super::StagedRuleCount {
                rule_name: "calculator-region".to_string(),
                node_count: 1,
            },
            super::StagedRuleCount {
                rule_name: "calculator-function".to_string(),
                node_count: function_count,
            },
        ]),
        diagnostics,
    }
}

fn parse_calculator_expression(source: &str, source_offset: usize) -> Option<FrontendExpression> {
    let mut parser = CalculatorExpressionParser {
        source,
        source_offset,
        cursor: 0,
    };
    let expression = parser.parse_additive()?;
    parser.skip_ws();
    (parser.cursor == source.len()).then_some(expression)
}

struct CalculatorExpressionParser<'source> {
    source: &'source str,
    source_offset: usize,
    cursor: usize,
}

impl CalculatorExpressionParser<'_> {
    fn parse_additive(&mut self) -> Option<FrontendExpression> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            self.skip_ws();
            let op = if self.eat_byte(b'+') {
                FrontendBinaryOp::Add
            } else if self.eat_byte(b'-') {
                FrontendBinaryOp::Sub
            } else {
                break;
            };
            let rhs = self.parse_multiplicative()?;
            let source_span = Span::new(expr_span(&lhs).start, expr_span(&rhs).end);
            lhs = FrontendExpression::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                source_span,
            };
        }
        Some(lhs)
    }

    fn parse_multiplicative(&mut self) -> Option<FrontendExpression> {
        let mut lhs = self.parse_primary()?;
        loop {
            self.skip_ws();
            if !self.eat_byte(b'*') {
                break;
            }
            let rhs = self.parse_primary()?;
            let source_span = Span::new(expr_span(&lhs).start, expr_span(&rhs).end);
            lhs = FrontendExpression::Binary {
                op: FrontendBinaryOp::Mul,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                source_span,
            };
        }
        Some(lhs)
    }

    fn parse_primary(&mut self) -> Option<FrontendExpression> {
        self.skip_ws();
        if self.eat_byte(b'(') {
            let expression = self.parse_additive()?;
            self.skip_ws();
            return self.eat_byte(b')').then_some(expression);
        }
        if self.peek_byte()?.is_ascii_digit() {
            return self.parse_integer();
        }
        if self.peek_byte()?.is_ascii_alphabetic() || self.peek_byte()? == b'_' {
            return self.parse_name_or_call();
        }
        None
    }

    fn parse_integer(&mut self) -> Option<FrontendExpression> {
        let start = self.cursor;
        while self.peek_byte().is_some_and(|byte| byte.is_ascii_digit()) {
            self.cursor += 1;
        }
        let text = &self.source[start..self.cursor];
        let value = text.parse().ok()?;
        Some(FrontendExpression::Integer {
            value,
            source_span: self.span(start, self.cursor),
        })
    }

    fn parse_name_or_call(&mut self) -> Option<FrontendExpression> {
        let start = self.cursor;
        while self
            .peek_byte()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'?'))
        {
            self.cursor += 1;
        }
        let name = &self.source[start..self.cursor];
        if !valid_calculator_function_name(name) {
            return None;
        }
        self.skip_ws();
        if !self.eat_byte(b'(') {
            return Some(FrontendExpression::Name {
                name: name.to_string(),
                source_span: self.span(start, self.cursor),
            });
        }
        let mut args = Vec::new();
        self.skip_ws();
        if !self.eat_byte(b')') {
            loop {
                args.push(self.parse_additive()?);
                self.skip_ws();
                if self.eat_byte(b')') {
                    break;
                }
                if !self.eat_byte(b',') {
                    return None;
                }
            }
        }
        Some(FrontendExpression::Call {
            callee: name.to_string(),
            args,
            source_span: self.span(start, self.cursor),
        })
    }

    fn skip_ws(&mut self) {
        while self
            .peek_byte()
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.cursor += 1;
        }
    }

    fn eat_byte(&mut self, byte: u8) -> bool {
        if self.peek_byte() == Some(byte) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.source.as_bytes().get(self.cursor).copied()
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.source_offset + start, self.source_offset + end)
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

pub(super) fn rules(grammar: GrammarId, grammar_slug: &str) -> Vec<GrammarRule> {
    parse_source_authored_grammar(CALCULATOR_GRAMMAR_SOURCE)
        .into_iter()
        .map(|rule| {
            let location = format!("source-grammar:{grammar_slug}/{}", rule.name);
            GrammarRule {
                id: GrammarRuleId::new(grammar, &rule.name),
                grammar,
                grammar_slug: grammar_slug.to_string(),
                name: rule.name.clone(),
                expression_data: grammar_rule_expression_data_from_expr(
                    &rule.name,
                    &rule.expression,
                    None,
                    &location,
                ),
                expression: rule.expression,
                projection: GrammarProjection {
                    origin: ProjectionOrigin::SourceAuthored,
                    location,
                    source_span: Some(rule.source_span),
                },
            }
        })
        .collect()
}
