use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use crate::diagnostic::Diagnostic;
use crate::source_data::{self, SourceData};

use super::interpreter::{
    GrammarActionValue, InterpretedParse, InterpretedParseError, evaluate_rule_action,
    interpret_region_result, interpreted_rule_counts, interpreted_rule_node_source_data,
    parse_source_authored_grammar,
};
use super::{
    CALCULATOR_GRAMMAR_NAME, GrammarId, GrammarProjection, GrammarRule, GrammarRuleId,
    ProjectionOrigin, StagedParseDriver, StagedParseOracle, StagedRegionEmission,
    grammar_rule_expression_data_from_expr, source_span_data, staged_action_error_diagnostic,
    staged_expected_diagnostic,
};

const CALCULATOR_GRAMMAR_RESOURCE: &str = "grammars/calculator.grammar";
const CALCULATOR_GRAMMAR_SOURCE: &str = include_str!("../../grammars/calculator.grammar");

pub(super) fn parse_region(body: &str, source_offset: usize) -> StagedRegionEmission {
    let grammar_rules = parse_source_authored_grammar(CALCULATOR_GRAMMAR_SOURCE);
    match interpret_calculator_region(&grammar_rules, body, source_offset) {
        Ok(parse) => emission_from_interpreted_parse(&grammar_rules, &parse),
        Err(error) => calculator_parse_error_emission(body, source_offset, error),
    }
}

fn interpret_calculator_region(
    rules: &[super::interpreter::SourceAuthoredRule],
    body: &str,
    source_offset: usize,
) -> Result<InterpretedParse, InterpretedParseError> {
    interpret_region_result(rules, "calculator-region", body, source_offset)
}

fn emission_from_interpreted_parse(
    grammar_rules: &[super::interpreter::SourceAuthoredRule],
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
        match evaluate_rule_action(grammar_rules, function) {
            Ok(Some(output)) => match output.value {
                GrammarActionValue::FrontendForm(form) => {
                    forms.push(form);
                    functions.push(output.data);
                }
                _ => diagnostics.push(staged_expected_diagnostic(
                    function.span.into(),
                    "calculator",
                    "frontend function action output",
                )),
            },
            Ok(None) => diagnostics.push(staged_expected_diagnostic(
                function.span.into(),
                "calculator",
                "calculator action",
            )),
            Err(error) => diagnostics.push(staged_action_error_diagnostic(error, "calculator")),
        }
    }
    if functions.is_empty() && diagnostics.is_empty() {
        diagnostics.push(staged_expected_diagnostic(
            parse.root.span.into(),
            "calculator",
            "`name = expression`",
        ));
    }
    StagedRegionEmission {
        forms,
        driver: StagedParseDriver::InterpretedGrammar,
        oracle: StagedParseOracle::None,
        data: SourceData::record(
            "calculator-region",
            vec![
                source_data::field("grammar", SourceData::symbol(CALCULATOR_GRAMMAR_NAME)),
                source_data::field(
                    "driver",
                    SourceData::symbol(StagedParseDriver::InterpretedGrammar.as_str()),
                ),
                source_data::field(
                    "oracle",
                    SourceData::symbol(StagedParseOracle::None.as_str()),
                ),
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
        driver: StagedParseDriver::InterpretedGrammar,
        oracle: StagedParseOracle::None,
        data: SourceData::record(
            "calculator-region",
            vec![
                source_data::field("grammar", SourceData::symbol(CALCULATOR_GRAMMAR_NAME)),
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
    staged_expected_diagnostic(error.span.into(), "calculator", &expected)
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
