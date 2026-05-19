use alloc::string::String;
use alloc::vec::Vec;

use super::interpreter::{
    InterpretedRuleNode, interpret_region_result, parse_source_authored_grammar,
};
use super::{ParserProductSummary, normalized_shadow_node_count};

const BASE_LOKO_SHADOW_GRAMMAR_SOURCE: &str =
    include_str!("../../grammars/base_loko_shadow.grammar");

pub(super) fn oracle_summary(source: &str) -> ParserProductSummary {
    let rules = parse_source_authored_grammar(BASE_LOKO_SHADOW_GRAMMAR_SOURCE);
    match interpret_region_result(&rules, "source-file", source, 0) {
        Ok(parse) => {
            let signature = shadow_top_level_signature(&parse.root);
            let top_level_count = signature.len();
            ParserProductSummary::with_top_level_signature(
                "source-authored-base-loko-shadow",
                normalized_shadow_node_count(top_level_count),
                top_level_count,
                signature.join(","),
                0,
                normalized_shadow_node_count(top_level_count),
            )
        }
        Err(_) => ParserProductSummary::with_top_level_signature(
            "source-authored-base-loko-shadow",
            0,
            0,
            String::new(),
            1,
            0,
        ),
    }
}

fn shadow_top_level_signature(root: &InterpretedRuleNode) -> Vec<&'static str> {
    root.children
        .iter()
        .filter(|child| child.rule_name == "top-level-definition")
        .filter_map(shadow_top_level_kind)
        .collect()
}

fn shadow_top_level_kind(node: &InterpretedRuleNode) -> Option<&'static str> {
    node.children
        .iter()
        .find_map(|child| match child.rule_name.as_str() {
            "class-definition" => Some("class-definition"),
            "generic-definition" => Some("generic-definition"),
            "method-definition" => Some("method-definition"),
            "function-definition" => Some("function-definition"),
            _ => None,
        })
}
