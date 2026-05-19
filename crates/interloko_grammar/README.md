# interloko_grammar

`interloko_grammar` is a small, `no_std` grammar kernel. It owns neutral grammar
expressions, grammar-source parsing, interpreted matching, parse trees, byte
spans, parse failures, and inert action descriptors.

It does not own Interloko syntax trees, diagnostics catalogs, staged-language
policy, semantic actions, runtime behavior, or source-data rendering. Callers
attach those meanings outside this crate.

## Quick Example

Parse a tiny grammar, match a source region, and inspect the parse tree:

```rust
use interloko_grammar::{Span, interpret_region, parse_grammar_source};

let grammar = "\
grammar records
rule field = name `:` type-name
rule name = [A-Za-z_]+
rule type-name = `<` name `>`
end
";

let rules = parse_grammar_source(grammar);
let parse = interpret_region(&rules, "field", "radius: <integer>", 100)
    .expect("field should parse");

assert_eq!(parse.root.rule_name, "field");
assert_eq!(parse.root.span, Span::new(100, 117));
assert_eq!(parse.root.text, "radius: <integer>");
assert!(parse.root.children.iter().any(|node| node.rule_name == "name"));
```

## Concepts

- **Grammar source**: a compact line-oriented format with `grammar`, `rule`, and
  `end` lines.
- **Rule expression**: structured grammar data such as rule references, literals,
  character classes, choices, repetitions, and transforms.
- **Parse tree**: a `RuleNode` tree produced by interpreted matching.
- **Action descriptor**: inert data attached with `=> action(...)`; this crate
  parses it but does not execute it.
- **Caller policy**: language-specific behavior, such as reserved words, supplied
  by the caller rather than built into this crate.

## Grammar Source

The source format is intentionally small:

```text
grammar tiny
rule item = (`a` | `b`) `!`?
rule items = item+
end
```

Supported expression forms include:

- rule references: `item`
- literals: `` `record` ``
- character classes: `[A-Za-z_]`
- sequence: `` name `:` type-name ``
- choice: `` `a` | `b` ``
- optional: `` `!`? ``
- one-or-more: `item+`
- zero-or-more: `item*`
- opaque regions: `` until(`end`) ``
- transforms: `item => emit-item(name: name)`

Transforms produce `TransformOutput` and optional `GrammarAction` values. They
are structural metadata only.

```rust
use interloko_grammar::{
    GrammarActionArg, GrammarActionTerm, parse_grammar_source,
};

let grammar = "\
grammar forms
rule form = name => emit-form(name: name, fields: field*, required: true)
end
";

let rules = parse_grammar_source(grammar);
let action = rules[0].action.as_ref().expect("action descriptor");

assert_eq!(action.name, "emit-form");
assert_eq!(
    action.args,
    vec![
        GrammarActionArg::new(
            "name",
            GrammarActionTerm::Capture { rule: "name".to_string() },
        ),
        GrammarActionArg::new(
            "fields",
            GrammarActionTerm::CaptureAll { rule: "field".to_string() },
        ),
        GrammarActionArg::new("required", GrammarActionTerm::Bool(true)),
    ],
);
```

## Parse Errors

Failures report a byte span and a list of expected fragments. The span uses the
`source_offset` supplied to `interpret_region`, so callers can parse embedded
regions while preserving coordinates in the larger source file.

```rust
use interloko_grammar::{Span, interpret_region, parse_grammar_source};

let rules = parse_grammar_source("grammar digits\nrule digits = [0-9]+\nend\n");
let error = interpret_region(&rules, "digits", "", 30)
    .expect_err("empty source is not digits");

assert_eq!(error.span, Span::new(30, 30));
assert_eq!(error.expected, vec!["[0-9]"]);
```

## Caller-Owned Policy

The kernel does not know about Interloko or any other language's keywords.
Identifier-like rules can reject complete matched text supplied by the caller:

```rust
use interloko_grammar::{Span, interpret_region, parse_grammar_source};

let rules = parse_grammar_source("grammar names\nrule name = [A-Za-z]+\nend\n")
    .into_iter()
    .map(|rule| rule.with_excluded_text(vec!["define".to_string()]))
    .collect::<Vec<_>>();

let error = interpret_region(&rules, "name", "define", 10)
    .expect_err("caller excluded this word");

assert_eq!(error.span, Span::new(10, 10));
assert_eq!(error.expected, vec!["name"]);
```

## Extension Points

The intended extension model is:

1. Use this crate to parse grammar source and match input.
2. Inspect `RuleNode` trees and `GrammarAction` descriptors.
3. Interpret actions in a caller-owned layer.

For Interloko, that caller-owned layer lives in `interloko_compiler`: it turns
grammar actions into frontend forms, CST data, diagnostics, and MOP/EOP
projection records. Another caller could attach a formatter, analyzer,
documentation generator, or different language frontend to the same neutral
parse-tree and action-descriptor data.

## Current Limits

- Grammar source parsing is intentionally small and line-oriented.
- Semantic actions are not executed here.
- There is no package loader or import system.
- There is no left-recursion support.
- Diagnostics are structural spans and expected fragments, not localized prose.
- Performance is tuned for clarity and current compiler/workbench use, not for a
  high-throughput parser generator.
