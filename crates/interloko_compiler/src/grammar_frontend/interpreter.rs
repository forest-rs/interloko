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

pub(super) const BASE_LOKO_GRAMMAR_SOURCE: &str = include_str!("../../grammars/base_loko.grammar");
pub(super) const DERIVED_HOST_GRAMMAR_SOURCE: &str =
    include_str!("../../grammars/derived_host.grammar");

const LOKO_RESERVED_WORDS: &[&str] = &[
    "define", "function", "class", "slot", "constant", "generic", "method", "let", "if", "then",
    "else", "handle", "with", "restart", "end",
];

fn apply_loko_reserved_words(rules: Vec<SourceAuthoredRule>) -> Vec<SourceAuthoredRule> {
    rules
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

pub(super) fn parse_source_authored_grammar(source: &'static str) -> CompiledGrammar {
    let (rules, diagnostics) = interloko_grammar::parse_grammar_source_with_diagnostics(source);
    assert!(
        diagnostics.is_empty(),
        "compiler-owned grammar produced source diagnostics: {diagnostics:?}",
    );
    CompiledGrammar::compile(apply_loko_reserved_words(rules))
}

/// Compiles the base-loko grammar from source.
///
/// Reserved-word exclusions are applied to the `name` rule before
/// compilation so identifier matches don't accept Loko keywords as
/// names. The result is fresh per call; callers that parse many files
/// should compile once and reuse the returned grammar.
pub(super) fn compile_base_loko_grammar() -> CompiledGrammar {
    parse_source_authored_grammar(BASE_LOKO_GRAMMAR_SOURCE)
}

/// Compiles the derived-host grammar by composing
/// [`compile_base_loko_grammar`] with [`DERIVED_HOST_GRAMMAR_SOURCE`].
///
/// Inheritance is resolved at compile time via
/// [`interloko_grammar::CompiledGrammar::try_compile_with_parent`], so
/// the returned grammar is interpreted just like a flat grammar.
/// Panics if `derived_host.grammar` produces source diagnostics or
/// inherited arity mismatches; both grammars ship with the compiler
/// and must always compile cleanly.
pub(super) fn compile_derived_host_grammar() -> CompiledGrammar {
    let parent = compile_base_loko_grammar();
    compile_derived_host_grammar_with_parent(&parent)
}

/// Compiles derived-host against a caller-supplied base grammar.
///
/// Used by call sites that already have a compiled base-loko grammar
/// in hand and want to avoid compiling it twice. The parent must be
/// the base-loko grammar; passing an unrelated grammar leaves the
/// derived-host references unresolved.
pub(super) fn compile_derived_host_grammar_with_parent(
    parent: &CompiledGrammar,
) -> CompiledGrammar {
    let parsed = interloko_grammar::parse_grammar_source_full(DERIVED_HOST_GRAMMAR_SOURCE);
    assert!(
        parsed.diagnostics.is_empty(),
        "derived_host.grammar produced source diagnostics: {:?}",
        parsed.diagnostics,
    );
    let rules = apply_loko_reserved_words(parsed.rules);
    CompiledGrammar::try_compile_with_parent(rules, parent)
        .expect("derived_host.grammar must compile against base-loko")
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

pub(super) fn owned_rule_node_source_data(node: &interloko_grammar::OwnedRuleNode) -> SourceData {
    let mut fields = vec![
        source_data::field("role", SourceData::symbol(node.role().as_str())),
        source_span_data(node.span),
        source_data::field("text", SourceData::string(node.text.as_str())),
    ];
    match &node.kind {
        interloko_grammar::OwnedRuleNodeKind::Rule { name } => {
            fields.push(source_data::field(
                "rule",
                SourceData::symbol(name.as_str()),
            ));
        }
        interloko_grammar::OwnedRuleNodeKind::Literal { value } => {
            fields.push(source_data::field(
                "literal",
                SourceData::string(value.as_str()),
            ));
        }
        interloko_grammar::OwnedRuleNodeKind::CharacterClass { value } => {
            fields.push(source_data::field(
                "character-class",
                SourceData::string(value.as_str()),
            ));
        }
        interloko_grammar::OwnedRuleNodeKind::OpaqueRegion { delimiter } => {
            fields.push(source_data::field(
                "opaque-delimiter",
                SourceData::string(delimiter.as_str()),
            ));
        }
        interloko_grammar::OwnedRuleNodeKind::RecoveryError => {}
        _ => {}
    }
    fields.push(source_data::field(
        "children",
        SourceData::list(
            node.children
                .iter()
                .map(owned_rule_node_source_data)
                .collect(),
        ),
    ));
    SourceData::record("rule-node", fields)
}

pub(super) fn interpreted_rule_node_source_data(node: &InterpretedRuleNode<'_>) -> SourceData {
    owned_rule_node_source_data(&node.to_owned_tree())
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
