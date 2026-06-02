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

assert_eq!(parse.root.name(), Some("field"));
assert_eq!(parse.root.span, Span::new(100, 117));
assert_eq!(parse.root.text, "radius: <integer>");
assert!(parse.root.children.iter().any(|node| node.is_rule("name")));
```

## Concepts

- **Grammar source**: a compact line-oriented format passed to
  [`parse_grammar_source`] with `grammar`, `rule`, and `end` lines.
- **Rule expression**: [`RuleExpression`] data such as rule references,
  literals, character classes, choices, repetitions, and transforms.
- **Parse tree**: a [`RuleNode`] tree returned by [`interpret_region`].
- **Node role**: [`RuleNodeRole`] classification that separates named grammar
  rules from anonymous literal, character-class, and opaque-region nodes.
- **Rule annotation**: inert [`RuleAnnotation`] metadata attached to a rule
  header with `@name` or `@name=value`.
- **Action descriptor**: inert [`GrammarAction`] data attached with
  `=> action(...)`; this crate parses it but does not execute it.
- **Caller policy**: language-specific behavior, such as reserved words, supplied
  by the caller through [`SourceAuthoredRule::with_excluded_text`] rather than
  built into this crate.

## Grammar Source

The source format is intentionally small:

```text
grammar tiny
rule word = `a` | `b`
rule maybe-word = word?
rule words = word+
end
```

A rule may declare a trivia mode between the name and `=`. Today only
`raw` is recognized; raw rules do not skip whitespace or `//` comments at
the rule boundary or before literals inside the rule body. Rule references
inside a raw rule still follow the called rule's own trivia mode.

```text
grammar strings
rule string-literal raw = `"` until(`"`) `"`
end
```

The same rule-header position also accepts inert annotations. Annotations are
preserved on [`SourceAuthoredRule`] for callers and tools, but the grammar
kernel does not interpret them while matching.

```text
grammar records
rule record-field @role=record-field @recovery-sync = name `:` type-name
end
```

Supported expression forms include:

- rule references: `item`
- literals: `` `record` ``
- character classes: `[A-Za-z_]`, with negation `[^a-z]`, escapes
  (`\\`, `\]`, `\-`, `\n`, `\r`, `\t`), and ASCII shorthand classes
  (`\d`, `\D`, `\w`, `\W`, `\s`, `\S`)
- sequence: `` name `:` type-name ``
- choice: `` `a` | `b` ``
- optional: `item?`
- one-or-more: `item+`
- zero-or-more: `item*`
- positive lookahead: `&item` (match without consuming)
- negative lookahead: `!item` (fail when `item` would match)
- committed-choice cut: `^item` (once `item` matches inside a `|` choice
  alternative, sibling alternatives are no longer tried)
- opaque regions: `` until(`end`) ``
- transforms: `item => emit-item(name: name)`

The lookahead and cut prefixes bind looser than the trailing repetition
operators, so `&item*` means `&(item*)`, not `(&item)*`. Whitespace between
the prefix and operand is not allowed; write `&item`, not `& item`. Cut's
commit is bounded by the innermost enclosing `|` choice: it does not bubble
past an optional, a repetition, an outer choice, or a rule boundary.

Transforms produce [`TransformOutput`] and optional [`GrammarAction`] values.
They are structural metadata only.

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
            GrammarActionTerm::Capture { rule: "name" },
        ),
        GrammarActionArg::new(
            "fields",
            GrammarActionTerm::CaptureAll { rule: "field" },
        ),
        GrammarActionArg::new("required", GrammarActionTerm::Bool(true)),
    ],
);
```

## Parse Tree Inspection

[`RuleNode`] has small traversal and query helpers so callers can inspect parse
trees without parsing presentation labels such as `literal::` or `class:A-Z`.

```rust
use interloko_grammar::{
    RuleNodeRole, Span, interpret_region, parse_grammar_source,
};

let rules = parse_grammar_source("\
grammar fields
rule field = name `:` type-name
rule name = [A-Za-z_]+
rule type-name = `<` name `>`
end
");

let parse = interpret_region(&rules, "field", "radius: <integer>", 100)
    .expect("field should parse");

assert_eq!(parse.root.role(), RuleNodeRole::NamedRule);
assert_eq!(parse.root.find_rules("name").len(), 2);

let colon = parse.root
    .find_by_role(RuleNodeRole::Literal)
    .into_iter()
    .find(|node| node.literal() == Some(":"))
    .expect("literal colon");
assert_eq!(colon.text, ":");

let integer = parse.root
    .smallest_covering_span(Span::new(109, 116))
    .expect("node covering integer");
assert_eq!(integer.name(), Some("name"));
assert_eq!(integer.text, "integer");
```

## Parse Errors

Failures return a [`ParseError`] with a [`Span`] and a list of expected
fragments. The span points at the unexpected source fragment when one exists,
or at a zero-width expectation point at EOF. It uses the `source_offset`
supplied to [`interpret_region`], so callers can parse embedded regions while
preserving coordinates in the larger source file.

```rust
use interloko_grammar::{
    ExpectedFragment, Span, interpret_region, parse_grammar_source,
};

let rules = parse_grammar_source("grammar digits\nrule digits = [0-9]+\nend\n");
let error = interpret_region(&rules, "digits", "", 30)
    .expect_err("empty source is not digits");

assert_eq!(error.span, Span::new(30, 30));
assert_eq!(error.expected, vec![ExpectedFragment::CharacterClass("0-9")]);
```

For tooling, [`inspect_region`] preserves a partial root when the root rule
matched but complete-region parsing failed on trailing input:

```rust
use interloko_grammar::{Span, inspect_region, parse_grammar_source};

let rules = parse_grammar_source("grammar one\nrule root = `ok`\nend\n");
let report = inspect_region(&rules, "root", "ok nope", 5);

assert!(!report.is_success());
assert_eq!(report.root.as_ref().map(|root| root.text), Some("ok"));
assert_eq!(report.error.as_ref().map(|error| error.span), Some(Span::new(8, 12)));
```

`inspect_region` and recovering parse reports also expose [`ParseMetrics`].
These counters are observational: they let callers see packrat memo hits,
misses, and final memo-table size for one parse without changing matching
semantics.

```rust
use interloko_grammar::{inspect_region, parse_grammar_source};

let rules = parse_grammar_source(
    "grammar tiny\n\
     rule root = item `x` | item `y`\n\
     rule item = [a]\n\
     end\n",
);
let report = inspect_region(&rules, "root", "ay", 0);

assert!(report.is_success());
assert_eq!(report.metrics.memo_hits, 1);
```

## Caller-Owned Policy

The kernel does not know about Interloko or any other language's keywords.
Identifier-like rules can reject complete matched text supplied by the caller:

```rust
use interloko_grammar::{
    ExpectedFragment, Span, interpret_region, parse_grammar_source,
};

let rules = parse_grammar_source("grammar names\nrule name = [A-Za-z]+\nend\n")
    .into_iter()
    .map(|rule| rule.with_excluded_text(vec!["define"]))
    .collect::<Vec<_>>();

let error = interpret_region(&rules, "name", "define", 10)
    .expect_err("caller excluded this word");

assert_eq!(error.span, Span::new(10, 16));
assert_eq!(error.expected, vec![ExpectedFragment::Rule("name")]);
```

## Extension Points

The intended extension model is:

1. Use [`parse_grammar_source`] to parse grammar source into
   [`SourceAuthoredRule`] values.
2. Use [`interpret_region`] to match input and return a [`Parse`] containing a
   [`RuleNode`] tree.
3. Use [`RuleNode::walk`], [`RuleNode::find_rules`], or
   [`RuleNode::find_by_role`] to inspect the matched tree.
4. Inspect [`RuleNode`] trees and [`GrammarAction`] descriptors.
5. Interpret actions in a caller-owned layer.

For Interloko, that caller-owned layer lives in `interloko_compiler`: it turns
grammar actions into frontend forms, CST data, diagnostics, and MOP/EOP
projection records. Another caller could attach a formatter, analyzer,
documentation generator, or different language frontend to the same neutral
parse-tree and action-descriptor data.

## Current Limits

- Grammar source parsing is intentionally small and line-oriented.
- Semantic actions are not executed here.
- There is no package loader or import system.
- Direct left recursion is supported; indirect left-recursive cycles are not.
- Partial parse reports are inspection reports, not parser recovery.
- Diagnostics are structural spans and expected fragments, not localized prose.
- Packrat memoization is per parse and in-memory only; there is no persistent
  parser cache across calls.

[`GrammarAction`]: crate::GrammarAction
[`Parse`]: crate::Parse
[`ParseError`]: crate::ParseError
[`ParseMetrics`]: crate::ParseMetrics
[`RuleExpression`]: crate::RuleExpression
[`RuleNode`]: crate::RuleNode
[`RuleNode::find_by_role`]: crate::RuleNode::find_by_role
[`RuleNode::find_rules`]: crate::RuleNode::find_rules
[`RuleNode::walk`]: crate::RuleNode::walk
[`RuleNodeRole`]: crate::RuleNodeRole
[`SourceAuthoredRule`]: crate::SourceAuthoredRule
[`SourceAuthoredRule::with_excluded_text`]: crate::SourceAuthoredRule::with_excluded_text
[`Span`]: crate::Span
[`TransformOutput`]: crate::TransformOutput
[`inspect_region`]: crate::inspect_region
[`interpret_region`]: crate::interpret_region
[`parse_grammar_source`]: crate::parse_grammar_source
