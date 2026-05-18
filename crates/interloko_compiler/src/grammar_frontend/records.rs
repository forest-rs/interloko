use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::diagnostic::{Diagnostic, DiagnosticArg};
use crate::source_data::{self, SourceData};
use crate::syntax::Span;

use super::interpreter::{
    InterpretedParse, InterpretedRuleNode, interpret_region, interpreted_rule_counts,
    interpreted_rule_node_source_data, parse_source_authored_grammar,
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

struct RecordBuilder {
    name: String,
    header_span: Span,
    fields: Vec<FrontendSlot>,
    field_data: Vec<SourceData>,
}

pub(super) fn parse_region(body: &str, source_offset: usize) -> StagedRegionEmission {
    if let Some(parse) = interpret_records_region(body, source_offset) {
        return emission_from_interpreted_parse(body, source_offset, &parse)
            .unwrap_or_else(|| parse_region_legacy(body, source_offset));
    }
    parse_region_legacy(body, source_offset)
}

fn interpret_records_region(body: &str, source_offset: usize) -> Option<InterpretedParse> {
    let rules = parse_source_authored_grammar(RECORDS_GRAMMAR_SOURCE);
    interpret_region(&rules, "records-region", body, source_offset)
}

fn emission_from_interpreted_parse(
    body: &str,
    source_offset: usize,
    parse: &InterpretedParse,
) -> Option<StagedRegionEmission> {
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
    Some(StagedRegionEmission {
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
    })
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

fn parse_region_legacy(body: &str, source_offset: usize) -> StagedRegionEmission {
    let mut forms = Vec::new();
    let mut diagnostics = Vec::new();
    let mut records = Vec::new();
    let mut current: Option<RecordBuilder> = None;
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

        if current.is_some() {
            if trimmed == "end" {
                let record = current.take().expect("record was just matched");
                let source_span = Span::new(record.header_span.start, trimmed_span.end);
                let fields_data = SourceData::list(record.field_data);
                forms.push(FrontendForm::class(
                    record.name.clone(),
                    "<object>",
                    record.fields,
                    source_span,
                ));
                records.push(SourceData::record(
                    "record-definition",
                    vec![
                        source_data::field("name", SourceData::symbol(record.name.as_str())),
                        source_span_data(source_span),
                        source_data::field("fields", fields_data),
                    ],
                ));
                continue;
            }
            let record = current.as_mut().expect("record was just matched");
            parse_record_field(trimmed, trimmed_span, record, &mut diagnostics);
            continue;
        }

        let Some(name) = trimmed.strip_prefix("record ") else {
            diagnostics.push(staged_expected_diagnostic(
                trimmed_span,
                "records",
                "`record name`",
            ));
            continue;
        };
        let name = name.trim();
        let Some(class_name) = normalize_record_class_name(name) else {
            diagnostics.push(staged_expected_diagnostic(
                trimmed_span,
                "records",
                "valid record name",
            ));
            continue;
        };
        current = Some(RecordBuilder {
            name: class_name,
            header_span: trimmed_span,
            fields: Vec::new(),
            field_data: Vec::new(),
        });
    }

    if let Some(record) = current {
        diagnostics.push(staged_expected_diagnostic(
            record.header_span,
            "records",
            "`end`",
        ));
    }

    if records.is_empty() && diagnostics.is_empty() {
        diagnostics.push(staged_expected_diagnostic(
            Span::new(source_offset, source_offset + body.len()),
            "records",
            "`record name`",
        ));
    }

    let record_count = forms.len();
    StagedRegionEmission {
        forms,
        data: SourceData::record(
            "records-region",
            vec![
                source_data::field("grammar", SourceData::symbol(RECORDS_GRAMMAR_NAME)),
                source_data::field("records", SourceData::list(records)),
            ],
        ),
        rule_counts: Vec::from([
            super::StagedRuleCount {
                rule_name: "records-region".to_string(),
                node_count: 1,
            },
            super::StagedRuleCount {
                rule_name: "record-definition".to_string(),
                node_count: record_count,
            },
        ]),
        diagnostics,
    }
}

fn parse_record_field(
    trimmed: &str,
    span: Span,
    record: &mut RecordBuilder,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some((name, type_name)) = trimmed.split_once(':') else {
        diagnostics.push(staged_expected_diagnostic(
            span,
            "records",
            "`field: <type>`",
        ));
        return;
    };
    let name = name.trim();
    let type_name = type_name.trim().trim_end_matches(';').trim();
    if !valid_record_field_name(name) {
        diagnostics.push(
            staged_expected_diagnostic(span, "records", "valid record field")
                .with_arg(DiagnosticArg::text("field", name)),
        );
        return;
    }
    if record.fields.iter().any(|field| field.name == name) {
        diagnostics.push(
            staged_expected_diagnostic(span, "records", "unique record field")
                .with_arg(DiagnosticArg::text("field", name)),
        );
        return;
    }
    if !valid_type_name(type_name) {
        diagnostics.push(
            staged_expected_diagnostic(span, "records", "field type name")
                .with_arg(DiagnosticArg::text("field", name)),
        );
        return;
    }

    record
        .fields
        .push(FrontendSlot::new(name, type_name, span).with_init_keyword(name, true));
    record.field_data.push(SourceData::record(
        "record-field",
        vec![
            source_data::field("name", SourceData::symbol(name)),
            source_data::field("type", SourceData::symbol(type_name)),
            source_span_data(span),
        ],
    ));
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
