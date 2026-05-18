use alloc::format;
use alloc::string::{String, ToString};
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
    GrammarId, GrammarProjection, GrammarRule, GrammarRuleId, ProjectionOrigin,
    RECORDS_GRAMMAR_NAME, StagedRegionEmission, grammar_rule_expression_data_from_expr,
    source_span_data, staged_expected_diagnostic,
};

const RECORDS_GRAMMAR_RESOURCE: &str = "grammars/records.grammar";
const RECORDS_GRAMMAR_SOURCE: &str = include_str!("../../grammars/records.grammar");

pub(super) fn parse_region(body: &str, source_offset: usize) -> StagedRegionEmission {
    match interpret_records_region(body, source_offset) {
        Ok(parse) => emission_from_interpreted_parse(body, source_offset, &parse),
        Err(error) => records_parse_error_emission(body, source_offset, error),
    }
}

fn interpret_records_region(
    body: &str,
    source_offset: usize,
) -> Result<InterpretedParse, InterpretedParseError> {
    let rules = parse_source_authored_grammar(RECORDS_GRAMMAR_SOURCE);
    interpret_region_result(&rules, "records-region", body, source_offset)
}

fn emission_from_interpreted_parse(
    body: &str,
    source_offset: usize,
    parse: &InterpretedParse,
) -> StagedRegionEmission {
    let mut forms = Vec::new();
    let mut diagnostics = Vec::new();
    let mut records = Vec::new();
    let grammar_rules = parse_source_authored_grammar(RECORDS_GRAMMAR_SOURCE);
    for record in parse
        .root
        .children
        .iter()
        .filter(|node| node.rule_name == "record-definition")
    {
        match evaluate_rule_action(&grammar_rules, record) {
            Ok(Some(output)) => match output.value {
                GrammarActionValue::FrontendForm(form) => {
                    forms.push(form);
                    records.push(output.data);
                }
                _ => diagnostics.push(staged_expected_diagnostic(
                    record.span,
                    "records",
                    "frontend class action output",
                )),
            },
            Ok(None) => diagnostics.push(staged_expected_diagnostic(
                record.span,
                "records",
                "record action",
            )),
            Err(error) => {
                let mut diagnostic =
                    staged_expected_diagnostic(error.span, "records", error.expected.as_ref());
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
    if records.is_empty() && diagnostics.is_empty() {
        diagnostics.push(staged_expected_diagnostic(
            Span::new(source_offset, source_offset + body.len()),
            "records",
            "`record name`",
        ));
    }
    StagedRegionEmission {
        forms,
        data: SourceData::record(
            "records-region",
            vec![
                source_data::field("grammar", SourceData::symbol(RECORDS_GRAMMAR_NAME)),
                source_data::field("driver", SourceData::symbol("interpreted-grammar")),
                source_data::field("parse-tree", interpreted_rule_node_source_data(&parse.root)),
                source_data::field("records", SourceData::list(records)),
            ],
        ),
        rule_counts: interpreted_rule_counts(&parse.root),
        diagnostics,
    }
}

fn records_parse_error_emission(
    body: &str,
    source_offset: usize,
    error: InterpretedParseError,
) -> StagedRegionEmission {
    let diagnostic = records_parse_error_diagnostic(body, source_offset, &error);
    StagedRegionEmission {
        forms: Vec::new(),
        data: SourceData::record(
            "records-region",
            vec![
                source_data::field("grammar", SourceData::symbol(RECORDS_GRAMMAR_NAME)),
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
                source_data::field("records", SourceData::list(Vec::new())),
            ],
        ),
        rule_counts: Vec::new(),
        diagnostics: Vec::from([diagnostic]),
    }
}

fn records_parse_error_diagnostic(
    body: &str,
    source_offset: usize,
    error: &InterpretedParseError,
) -> Diagnostic {
    let expected = joined_expected(error);
    staged_expected_diagnostic(
        parse_error_span(body, source_offset, error.span),
        "records",
        &expected,
    )
}

fn joined_expected(error: &InterpretedParseError) -> String {
    if error.expected.is_empty() {
        "records grammar".to_string()
    } else {
        error.expected.join(" or ")
    }
}

fn parse_error_span(body: &str, source_offset: usize, span: Span) -> Span {
    if span.start == span.end && span.start == source_offset {
        Span::new(source_offset, source_offset + body.len())
    } else {
        span
    }
}

pub(super) fn rules(grammar: GrammarId, grammar_slug: &str) -> Vec<GrammarRule> {
    parse_source_authored_grammar(RECORDS_GRAMMAR_SOURCE)
        .into_iter()
        .map(|rule| {
            let location = format!("resource:{RECORDS_GRAMMAR_RESOURCE}/{}", rule.name);
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
                action: rule.action,
                projection: GrammarProjection {
                    origin: ProjectionOrigin::SourceAuthored,
                    location,
                    source_span: Some(rule.source_span),
                },
            }
        })
        .collect()
}
