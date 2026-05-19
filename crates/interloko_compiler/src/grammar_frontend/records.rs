use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::diagnostic::Diagnostic;
use crate::source_data::{self, SourceData};
use crate::syntax::Span;

use super::actions::{GrammarActionValue, evaluate_rule_action};
use super::interpreter::{
    InterpretedParse, InterpretedParseError, interpret_region_result,
    interpreted_rule_applications, interpreted_rule_counts, interpreted_rule_node_source_data,
    parse_source_authored_grammar,
};
use super::{
    GrammarId, GrammarProjection, GrammarRule, GrammarRuleId, ProjectionOrigin,
    RECORDS_GRAMMAR_NAME, StagedParseDriver, StagedParseOracle, StagedRegionEmission,
    grammar_rule_expression_data_from_expr, source_span_data, staged_action_error_diagnostic,
    staged_expected_diagnostic,
};

const RECORDS_GRAMMAR_RESOURCE: &str = "grammars/records.grammar";
const RECORDS_GRAMMAR_SOURCE: &str = include_str!("../../grammars/records.grammar");

pub(super) fn parse_region(body: &str, source_offset: usize) -> StagedRegionEmission {
    let grammar_rules = parse_source_authored_grammar(RECORDS_GRAMMAR_SOURCE);
    match interpret_records_region(&grammar_rules, body, source_offset) {
        Ok(parse) => emission_from_interpreted_parse(&grammar_rules, &parse),
        Err(error) => records_parse_error_emission(body, source_offset, error),
    }
}

fn interpret_records_region<'s>(
    rules: &[super::interpreter::SourceAuthoredRule],
    body: &'s str,
    source_offset: usize,
) -> Result<InterpretedParse<'s>, InterpretedParseError> {
    interpret_region_result(rules, "records-region", body, source_offset)
}

fn emission_from_interpreted_parse(
    grammar_rules: &[super::interpreter::SourceAuthoredRule],
    parse: &InterpretedParse<'_>,
) -> StagedRegionEmission {
    let mut forms = Vec::new();
    let mut diagnostics = Vec::new();
    let mut records = Vec::new();
    for record in parse
        .root
        .children
        .iter()
        .filter(|node| node.is_rule("record-definition"))
    {
        match evaluate_rule_action(grammar_rules, record) {
            Ok(Some(output)) => match output.value {
                GrammarActionValue::FrontendForm(form) => {
                    forms.push(form);
                    records.push(output.data);
                }
                _ => diagnostics.push(staged_expected_diagnostic(
                    record.span.into(),
                    "records",
                    "frontend class action output",
                )),
            },
            Ok(None) => diagnostics.push(staged_expected_diagnostic(
                record.span.into(),
                "records",
                "record action",
            )),
            Err(error) => diagnostics.push(staged_action_error_diagnostic(error, "records")),
        }
    }
    if records.is_empty() && diagnostics.is_empty() {
        diagnostics.push(staged_expected_diagnostic(
            parse.root.span.into(),
            "records",
            "`record name`",
        ));
    }
    StagedRegionEmission {
        forms,
        driver: StagedParseDriver::InterpretedGrammar,
        oracle: StagedParseOracle::None,
        data: SourceData::record(
            "records-region",
            vec![
                source_data::field("grammar", SourceData::symbol(RECORDS_GRAMMAR_NAME)),
                source_data::field(
                    "driver",
                    SourceData::symbol(StagedParseDriver::InterpretedGrammar.as_str()),
                ),
                source_data::field(
                    "oracle",
                    SourceData::symbol(StagedParseOracle::None.as_str()),
                ),
                source_data::field("parse-tree", interpreted_rule_node_source_data(&parse.root)),
                source_data::field("records", SourceData::list(records)),
            ],
        ),
        rule_counts: interpreted_rule_counts(&parse.root),
        rule_applications: interpreted_rule_applications(&parse.root),
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
        driver: StagedParseDriver::InterpretedGrammar,
        oracle: StagedParseOracle::None,
        data: SourceData::record(
            "records-region",
            vec![
                source_data::field("grammar", SourceData::symbol(RECORDS_GRAMMAR_NAME)),
                source_data::field(
                    "driver",
                    SourceData::symbol(StagedParseDriver::InterpretedGrammar.as_str()),
                ),
                source_data::field(
                    "oracle",
                    SourceData::symbol(StagedParseOracle::None.as_str()),
                ),
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
                                        .map(|item| SourceData::string(format!("{item}")))
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
        rule_applications: Vec::new(),
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
        error
            .expected
            .iter()
            .map(|expected| format!("{expected}"))
            .collect::<Vec<_>>()
            .join(" or ")
    }
}

fn parse_error_span(body: &str, source_offset: usize, span: interloko_grammar::Span) -> Span {
    if span.start == span.end && span.start == source_offset {
        Span::new(source_offset, source_offset + body.len())
    } else {
        span.into()
    }
}

pub(super) fn rules(grammar: GrammarId, grammar_slug: &str) -> Vec<GrammarRule> {
    parse_source_authored_grammar(RECORDS_GRAMMAR_SOURCE)
        .into_iter()
        .map(|rule| {
            let location = format!("resource:{RECORDS_GRAMMAR_RESOURCE}/{}", rule.name);
            GrammarRule {
                id: GrammarRuleId::new(grammar, rule.name),
                grammar,
                grammar_slug: grammar_slug.to_string(),
                name: rule.name.to_string(),
                expression_data: grammar_rule_expression_data_from_expr(
                    rule.name,
                    &rule.expression,
                    None,
                    &location,
                ),
                expression: rule.expression,
                action: rule.action,
                projection: GrammarProjection {
                    origin: ProjectionOrigin::SourceAuthored,
                    location,
                    source_span: Some(rule.source_span.into()),
                },
            }
        })
        .collect()
}
