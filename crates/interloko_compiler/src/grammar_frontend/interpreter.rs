use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

pub(super) type InterpretedParse<'s> = interloko_grammar::Parse<'static, 's>;
pub(super) type InterpretedParseError = interloko_grammar::ParseError<'static>;
pub(super) type InterpretedRuleNode<'s> = interloko_grammar::RuleNode<'static, 's>;
pub(super) type SourceAuthoredRule = interloko_grammar::SourceAuthoredRule<'static>;
pub(super) type CompiledGrammar = interloko_grammar::CompiledGrammar<'static>;

use crate::source_data::{self, SourceData};

use super::{StagedRuleApplication, StagedRuleCount, source_span_data};

const LOKO_RESERVED_WORDS: &[&str] = &[
    "define", "function", "class", "slot", "constant", "generic", "method", "let", "if", "then",
    "else", "handle", "with", "restart", "end",
];

pub(super) fn parse_source_authored_grammar(source: &'static str) -> CompiledGrammar {
    let (rules, diagnostics) = interloko_grammar::parse_grammar_source_with_diagnostics(source);
    assert!(
        diagnostics.is_empty(),
        "compiler-owned grammar produced source diagnostics: {diagnostics:?}",
    );
    let rules = rules
        .into_iter()
        .map(|rule| {
            if rule.name == "name" {
                rule.with_excluded_text(LOKO_RESERVED_WORDS.to_vec())
            } else {
                rule
            }
        })
        .collect();
    CompiledGrammar::compile(rules)
}

pub(super) fn interpret_region_result<'s>(
    grammar: &CompiledGrammar,
    root_rule: &'static str,
    source: &'s str,
    source_offset: usize,
) -> Result<InterpretedParse<'s>, InterpretedParseError> {
    interloko_grammar::interpret_region_compiled(grammar, root_rule, source, source_offset)
}

pub(super) fn interpret_rule_error(
    grammar: &CompiledGrammar,
    rule_name: &'static str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> InterpretedParseError {
    interloko_grammar::interpret_rule_error_compiled(
        grammar,
        rule_name,
        source,
        position,
        source_offset,
    )
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

pub(super) fn interpreted_rule_applications(
    root: &InterpretedRuleNode<'_>,
) -> Vec<StagedRuleApplication> {
    root.walk()
        .filter_map(|node| {
            Some(StagedRuleApplication {
                rule_name: node.name()?.to_string(),
                span: node.span.into(),
            })
        })
        .collect()
}
