use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use crate::diagnostic::{Diagnostic, DiagnosticArg};
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
    source_span_data, staged_expected_diagnostic,
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
            Err(error) => {
                let mut diagnostic =
                    staged_expected_diagnostic(error.span, "calculator", error.expected.as_ref());
                if let Some(rule) = error.rule {
                    diagnostic = diagnostic.with_arg(DiagnosticArg::text("rule", rule.to_string()));
                }
                if let Some(action) = error.action {
                    diagnostic =
                        diagnostic.with_arg(DiagnosticArg::text("action", action.to_string()));
                }
                if let Some(arg) = error.arg {
                    diagnostic =
                        diagnostic.with_arg(DiagnosticArg::text("argument", arg.to_string()));
                }
                if let Some(field) = error.field {
                    diagnostic =
                        diagnostic.with_arg(DiagnosticArg::text("field", field.to_string()));
                }
                diagnostics.push(diagnostic);
            }
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
    _body: &str,
    _source_offset: usize,
    error: &InterpretedParseError,
) -> Diagnostic {
    let expected = if error.expected.is_empty() {
        "calculator grammar".to_string()
    } else {
        error.expected.join(" or ")
    };
    staged_expected_diagnostic(error.span, "calculator", &expected)
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
