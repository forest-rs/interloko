use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

pub(super) type InterpretedParse<'s> = interloko_grammar::Parse<'static, 's>;
pub(super) type InterpretedParseError = interloko_grammar::ParseError<'static>;
pub(super) type InterpretedRuleNode<'s> = interloko_grammar::RuleNode<'static, 's>;
pub(super) type SourceAuthoredRule = interloko_grammar::SourceAuthoredRule<'static>;

use crate::source_data::{self, SourceData};

use super::{StagedRuleCount, source_span_data};

const LOKO_RESERVED_WORDS: &[&str] = &[
    "define", "function", "class", "slot", "constant", "generic", "method", "let", "if", "then",
    "else", "handle", "with", "restart", "end",
];

pub(super) fn parse_source_authored_grammar(source: &'static str) -> Vec<SourceAuthoredRule> {
    interloko_grammar::parse_grammar_source(source)
        .into_iter()
        .map(|rule| {
            if rule.name == "name" {
                rule.with_excluded_text(LOKO_RESERVED_WORDS.to_vec())
            } else {
                rule
            }
        })
        .collect()
}

pub(super) fn interpret_region_result<'s>(
    rules: &[SourceAuthoredRule],
    root_rule: &'static str,
    source: &'s str,
    source_offset: usize,
) -> Result<InterpretedParse<'s>, InterpretedParseError> {
    interloko_grammar::interpret_region(rules, root_rule, source, source_offset)
}

pub(super) fn interpret_rule_error(
    rules: &[SourceAuthoredRule],
    rule_name: &'static str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> InterpretedParseError {
    interloko_grammar::interpret_rule_error(rules, rule_name, source, position, source_offset)
}

pub(super) fn interpreted_rule_node_source_data(node: &InterpretedRuleNode<'_>) -> SourceData {
    let mut fields = vec![
        source_data::field("role", SourceData::symbol(node.role().as_str())),
        source_span_data(node.span),
        source_data::field("text", SourceData::string(node.text)),
    ];
    if let Some(name) = node.name() {
        fields.push(source_data::field("rule", SourceData::symbol(name)));
    }
    if let Some(literal) = node.literal() {
        fields.push(source_data::field("literal", SourceData::string(literal)));
    }
    if let Some(class) = node.character_class() {
        fields.push(source_data::field(
            "character-class",
            SourceData::string(class),
        ));
    }
    if let Some(delimiter) = node.opaque_delimiter() {
        fields.push(source_data::field(
            "opaque-delimiter",
            SourceData::string(delimiter),
        ));
    }
    fields.push(source_data::field(
        "children",
        SourceData::list(
            node.children
                .iter()
                .map(interpreted_rule_node_source_data)
                .collect(),
        ),
    ));
    SourceData::record("rule-node", fields)
}

pub(super) fn interpreted_rule_counts(root: &InterpretedRuleNode<'_>) -> Vec<StagedRuleCount> {
    interloko_grammar::named_rule_counts(root)
        .into_iter()
        .filter_map(|count| {
            Some(StagedRuleCount {
                rule_name: count.rule_name()?.to_string(),
                node_count: count.node_count,
            })
        })
        .collect()
}
