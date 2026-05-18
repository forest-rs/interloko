use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::diagnostic::{Diagnostic, DiagnosticArg};
use crate::source_data::{self, SourceData};
use crate::syntax::Span;

use super::interpreter::{
    InterpretedParse, InterpretedParseError, InterpretedRuleNode, interpret_region_result,
    interpreted_rule_counts, interpreted_rule_node_source_data, parse_source_authored_grammar,
};
use super::{
    FrontendForm, FrontendSlot, GrammarId, GrammarProjection, GrammarRule, GrammarRuleId,
    ProjectionOrigin, RECORDS_GRAMMAR_NAME, StagedRegionEmission,
    grammar_rule_expression_data_from_expr, normalize_record_class_name, source_span_data,
    staged_expected_diagnostic, valid_record_field_name, valid_type_name,
};

const RECORDS_GRAMMAR_SOURCE: &str = "\
grammar records\n\
rule records-region = record-definition*\n\
rule record-definition = `record` name record-field* `end` => frontend-class\n\
rule record-field = name `:` type-name `;`? => frontend-slot\n\
end\n";

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
    for record in parse
        .root
        .children
        .iter()
        .filter(|node| node.rule_name == "record-definition")
    {
        let Some((form, data)) = frontend_record_from_interpreted_node(record, &mut diagnostics)
        else {
            continue;
        };
        forms.push(form);
        records.push(data);
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

fn frontend_record_from_interpreted_node(
    record: &InterpretedRuleNode,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(FrontendForm, SourceData)> {
    let name = record
        .children
        .iter()
        .find(|node| node.rule_name == "name")?;
    let Some(class_name) = normalize_record_class_name(name.text.trim()) else {
        diagnostics.push(staged_expected_diagnostic(
            name.span,
            "records",
            "valid record name",
        ));
        return None;
    };
    let mut fields = Vec::new();
    let mut field_data = Vec::new();
    for field in record
        .children
        .iter()
        .filter(|node| node.rule_name == "record-field")
    {
        parse_interpreted_record_field(field, &mut fields, &mut field_data, diagnostics);
    }
    let fields_data = SourceData::list(field_data);
    Some((
        FrontendForm::class(class_name.clone(), "<object>", fields, record.span),
        SourceData::record(
            "record-definition",
            vec![
                source_data::field("name", SourceData::symbol(class_name.as_str())),
                source_span_data(record.span),
                source_data::field("fields", fields_data),
            ],
        ),
    ))
}

fn parse_interpreted_record_field(
    field: &InterpretedRuleNode,
    fields: &mut Vec<FrontendSlot>,
    field_data: &mut Vec<SourceData>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(name) = field.children.iter().find(|node| node.rule_name == "name") else {
        diagnostics.push(staged_expected_diagnostic(
            field.span,
            "records",
            "`field: <type>`",
        ));
        return;
    };
    let Some(type_name) = field
        .children
        .iter()
        .find(|node| node.rule_name == "type-name")
    else {
        diagnostics.push(staged_expected_diagnostic(
            field.span,
            "records",
            "`field: <type>`",
        ));
        return;
    };
    let name_text = name.text.trim();
    let type_text = type_name.text.trim();
    if !valid_record_field_name(name_text) {
        diagnostics.push(
            staged_expected_diagnostic(name.span, "records", "valid record field")
                .with_arg(DiagnosticArg::text("field", name_text)),
        );
        return;
    }
    if fields.iter().any(|field| field.name == name_text) {
        diagnostics.push(
            staged_expected_diagnostic(name.span, "records", "unique record field")
                .with_arg(DiagnosticArg::text("field", name_text)),
        );
        return;
    }
    if !valid_type_name(type_text) {
        diagnostics.push(
            staged_expected_diagnostic(type_name.span, "records", "field type name")
                .with_arg(DiagnosticArg::text("field", name_text)),
        );
        return;
    }
    fields.push(
        FrontendSlot::new(name_text, type_text, field.span).with_init_keyword(name_text, true),
    );
    field_data.push(SourceData::record(
        "record-field",
        vec![
            source_data::field("name", SourceData::symbol(name_text)),
            source_data::field("type", SourceData::symbol(type_text)),
            source_span_data(field.span),
        ],
    ));
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
    if let Some(diagnostic) = record_shape_diagnostic(body, source_offset) {
        return diagnostic;
    }
    let expected = joined_expected(error);
    staged_expected_diagnostic(
        parse_error_span(body, source_offset, error.span),
        "records",
        &expected,
    )
}

fn record_shape_diagnostic(body: &str, source_offset: usize) -> Option<Diagnostic> {
    let mut in_record = false;
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
        if !in_record {
            if trimmed.starts_with("record ") {
                in_record = true;
                continue;
            }
            return Some(staged_expected_diagnostic(span, "records", "`record name`"));
        }
        if trimmed == "end" {
            in_record = false;
            continue;
        }
        let Some((name, _)) = trimmed.split_once(':') else {
            return Some(staged_expected_diagnostic(
                span,
                "records",
                "`field: <type>`",
            ));
        };
        let name = name.trim();
        if !valid_record_field_name(name) {
            return Some(
                staged_expected_diagnostic(span, "records", "valid record field")
                    .with_arg(DiagnosticArg::text("field", name)),
            );
        }
        return Some(staged_expected_diagnostic(
            span,
            "records",
            "`field: <type>`",
        ));
    }
    None
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
