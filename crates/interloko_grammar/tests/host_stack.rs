//! Recursive matching must fit ordinary host thread stacks in debug builds.

use interloko_grammar::{Span, interpret_region, parse_grammar_source};

#[test]
fn consuming_recursion_fits_a_two_megabyte_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let grammar = "grammar nested\nrule root = `(` root `)` | `x`\nend\n";
            let rules = parse_grammar_source(grammar);
            let source = format!("{}x{}", "(".repeat(96), ")".repeat(96));
            let parsed = interpret_region(&rules, "root", &source, 37).expect("nested source");
            assert_eq!(parsed.root.text, source);
            assert_eq!(parsed.root.span, Span::new(37, 37 + source.len()));
        })
        .expect("spawn parsing thread")
        .join()
        .expect("parsing thread completed");
}
