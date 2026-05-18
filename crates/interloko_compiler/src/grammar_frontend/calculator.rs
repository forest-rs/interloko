use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use crate::diagnostic::Diagnostic;
use crate::source_data::{self, SourceData};
use crate::syntax::Span;

use super::interpreter::{
    GrammarActionValue, InterpretedParse, InterpretedParseError, evaluate_rule_action,
    interpret_region_result, interpreted_rule_counts, interpreted_rule_node_source_data,
    parse_source_authored_grammar,
};
use super::{
    CALCULATOR_GRAMMAR_NAME, GrammarId, GrammarProjection, GrammarRule, GrammarRuleId,
    ProjectionOrigin, StagedRegionEmission, grammar_rule_expression_data_from_expr,
    source_span_data, staged_expected_diagnostic, valid_calculator_function_name,
};

const CALCULATOR_GRAMMAR_RESOURCE: &str = "grammars/calculator.grammar";
const CALCULATOR_GRAMMAR_SOURCE: &str = include_str!("../../grammars/calculator.grammar");

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
    let grammar_rules = parse_source_authored_grammar(CALCULATOR_GRAMMAR_SOURCE);
    for function in parse
        .root
        .children
        .iter()
        .filter(|node| node.rule_name == "calculator-function")
    {
        match evaluate_rule_action(&grammar_rules, function) {
            Ok(Some(output)) => match output.value {
                GrammarActionValue::FrontendForm(form) => {
                    forms.push(form);
                    functions.push(output.data);
                }
                _ => diagnostics.push(staged_expected_diagnostic(
                    function.span,
                    "calculator",
                    "frontend function action output",
                )),
            },
            Ok(None) => diagnostics.push(staged_expected_diagnostic(
                function.span,
                "calculator",
                "calculator action",
            )),
            Err(error) => diagnostics.push(staged_expected_diagnostic(
                error.span,
                "calculator",
                &error.expected,
            )),
        }
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

pub(super) fn rules(grammar: GrammarId, grammar_slug: &str) -> Vec<GrammarRule> {
    parse_source_authored_grammar(CALCULATOR_GRAMMAR_SOURCE)
        .into_iter()
        .map(|rule| {
            let location = format!("resource:{CALCULATOR_GRAMMAR_RESOURCE}/{}", rule.name);
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
