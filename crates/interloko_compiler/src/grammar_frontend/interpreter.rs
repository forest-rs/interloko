use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

pub(super) use interloko_grammar::{
    Parse as InterpretedParse, ParseError as InterpretedParseError,
    RuleNode as InterpretedRuleNode, SourceAuthoredRule,
};

use crate::source_data::{self, SourceData};

use super::{StagedRuleCount, source_span_data};

const LOKO_RESERVED_WORDS: &[&str] = &[
    "define", "function", "class", "slot", "constant", "generic", "method", "let", "if", "then",
    "else", "handle", "with", "restart", "end",
];

pub(super) fn parse_source_authored_grammar(source: &str) -> Vec<SourceAuthoredRule> {
    interloko_grammar::parse_grammar_source(source)
        .into_iter()
        .map(|rule| {
            if rule.name == "name" {
                rule.with_excluded_text(
                    LOKO_RESERVED_WORDS
                        .iter()
                        .map(|word| (*word).to_string())
                        .collect(),
                )
            } else {
                rule
            }
        })
        .collect()
}

pub(super) fn interpret_region_result(
    rules: &[SourceAuthoredRule],
    root_rule: &str,
    source: &str,
    source_offset: usize,
) -> Result<InterpretedParse, InterpretedParseError> {
    interloko_grammar::interpret_region(rules, root_rule, source, source_offset)
}

pub(super) fn interpret_rule_error(
    rules: &[SourceAuthoredRule],
    rule_name: &str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> InterpretedParseError {
    interloko_grammar::interpret_rule_error(rules, rule_name, source, position, source_offset)
}

pub(super) fn interpreted_rule_node_source_data(node: &InterpretedRuleNode) -> SourceData {
    SourceData::record(
        "rule-node",
        vec![
            source_data::field("rule", SourceData::symbol(node.rule_name.as_str())),
            source_span_data(node.span),
            source_data::field(
                "children",
                SourceData::list(
                    node.children
                        .iter()
                        .map(interpreted_rule_node_source_data)
                        .collect(),
                ),
            ),
        ],
    )
}

pub(super) fn interpreted_rule_counts(root: &InterpretedRuleNode) -> Vec<StagedRuleCount> {
    interloko_grammar::rule_counts(root)
        .into_iter()
        .map(|count| StagedRuleCount {
            rule_name: count.rule_name,
            node_count: count.node_count,
        })
        .collect()
}
