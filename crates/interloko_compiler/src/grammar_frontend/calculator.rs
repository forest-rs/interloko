use alloc::boxed::Box;
use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use crate::diagnostic::Diagnostic;
use crate::source_data::{self, SourceData};
use crate::syntax::Span;

use super::interpreter::{
    InterpretedParse, InterpretedParseError, InterpretedRuleNode, interpret_region_result,
    interpreted_rule_counts, interpreted_rule_node_source_data, parse_source_authored_grammar,
};
use super::{
    CALCULATOR_GRAMMAR_NAME, FrontendBinaryOp, FrontendExpression, FrontendForm, GrammarId,
    GrammarProjection, GrammarRule, GrammarRuleId, ProjectionOrigin, StagedRegionEmission,
    grammar_rule_expression_data_from_expr, source_span_data, staged_expected_diagnostic,
    valid_calculator_function_name,
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
    match interpret_calculator_region(body, source_offset) {
        Ok(parse) => emission_from_interpreted_parse(body, source_offset, &parse),
        Err(error) => calculator_parse_error_emission(body, source_offset, error),
    }
}

fn interpret_calculator_region(
    body: &str,
    source_offset: usize,
) -> Result<InterpretedParse, InterpretedParseError> {
    let rules = parse_source_authored_grammar(CALCULATOR_GRAMMAR_SOURCE);
    interpret_region_result(&rules, "calculator-region", body, source_offset)
}

fn emission_from_interpreted_parse(
    body: &str,
    source_offset: usize,
    parse: &InterpretedParse,
) -> StagedRegionEmission {
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
    StagedRegionEmission {
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
    }
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

fn calculator_parse_error_emission(
    body: &str,
    source_offset: usize,
    error: InterpretedParseError,
) -> StagedRegionEmission {
    let diagnostic = calculator_parse_error_diagnostic(body, source_offset, &error);
    StagedRegionEmission {
        forms: Vec::new(),
        data: SourceData::record(
            "calculator-region",
            vec![
                source_data::field("grammar", SourceData::symbol(CALCULATOR_GRAMMAR_NAME)),
                source_data::field("driver", SourceData::symbol("interpreted-grammar")),
                source_data::field(
                    "parse-error",
                    SourceData::record(
                        "parse-error",
                        vec![
                            source_span_data(error.span),
                            source_data::field(
                                "expected",
                                SourceData::list(
                                    error
                                        .expected
                                        .iter()
                                        .map(|item| SourceData::string(item.as_str()))
                                        .collect(),
                                ),
                            ),
                        ],
                    ),
                ),
                source_data::field("functions", SourceData::list(Vec::new())),
            ],
        ),
        rule_counts: Vec::new(),
        diagnostics: Vec::from([diagnostic]),
    }
}

fn calculator_parse_error_diagnostic(
    body: &str,
    source_offset: usize,
    error: &InterpretedParseError,
) -> Diagnostic {
    if let Some(diagnostic) = calculator_shape_diagnostic(body, source_offset) {
        return diagnostic;
    }
    let expected = if error.expected.is_empty() {
        "calculator grammar".to_string()
    } else {
        error.expected.join(" or ")
    };
    staged_expected_diagnostic(error.span, "calculator", &expected)
}

fn calculator_shape_diagnostic(body: &str, source_offset: usize) -> Option<Diagnostic> {
    let mut offset = source_offset;
    for line in body.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let leading = line.find(trimmed).unwrap_or(0);
        let span = Span::new(line_start + leading, line_start + leading + trimmed.len());
        let Some((name, expression)) = trimmed.split_once('=') else {
            return Some(staged_expected_diagnostic(
                span,
                "calculator",
                "`name = expression`",
            ));
        };
        if name.trim().is_empty()
            || expression.trim().is_empty()
            || !valid_calculator_function_name(name.trim())
        {
            return Some(staged_expected_diagnostic(
                span,
                "calculator",
                "`name = expression`",
            ));
        }
        let expression_offset = trimmed.find(expression.trim()).unwrap_or(0);
        let expression_start = span.start + expression_offset;
        return Some(staged_expected_diagnostic(
            Span::new(expression_start, expression_start + expression.trim().len()),
            "calculator",
            "calculator expression",
        ));
    }
    None
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
