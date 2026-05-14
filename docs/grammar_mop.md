# Grammar MOP

Interloko exposes the current hand-written Loko parser as read-only grammar
metaobjects. This is a projection layer, not a parser rewrite and not a macro
system.

## Boundary

`interloko_compiler::grammar_frontend` owns language-environment identity,
grammar metaobjects, grammar-stack resolution, parser invocation, parse
activations, and parse provenance.

It explicitly does not own AST lowering, semantic lowering, source-authored
grammars, semantic actions, staged parsing, host code execution, OMeta/Ohm
compatibility, or user-defined parser rules.

## Current Records

The default projection names one language environment and one grammar:

```text
language-environment:base-loko  BaseLokoEnvironment
grammar:base-loko               BaseLokoGrammar
```

`BaseLokoGrammar` contains direct and effective grammar-rule records projected
from the current parser surface, such as:

```text
grammar-rule:base-loko/source-file
grammar-rule:base-loko/class-definition
grammar-rule:base-loko/function-definition
grammar-rule:base-loko/method-definition
grammar-rule:base-loko/expression
effective-grammar-rule:base-loko/function-definition
```

Each rule records:

- a stable rule id;
- the owning grammar id;
- a projected rule expression;
- inert source data describing the projected rule in a structured form;
- projection origin;
- the parser implementation location, for example
  `syntax.rs:Parser::parse_function`;
- optional source span information, currently `none` because these rules are
  compiler-projected Rust facts rather than source-authored grammar forms.

There is also a trusted host fixture:

```text
language-environment:derived-host  DerivedHostEnvironment
grammar:derived-host               DerivedHostGrammar extends BaseLokoGrammar
```

`DerivedHostGrammar` overrides the projected `function-definition` rule and
inherits every other effective rule from `BaseLokoGrammar`. This proves direct
and effective rule records can be inspected separately. It still delegates CST
construction to the base parser; it is a grammar-stack/provenance fixture, not
new syntax.

## Parse Reports

The grammar frontend wraps the existing `syntax::parse_file` path and returns a
`GrammarParseReport`.

That report contains:

- the parsed `SyntaxTree`;
- the resolved language environment;
- the resolved grammar stack;
- projected grammar, direct-rule, and effective-rule records;
- grammar activations over source regions;
- parse provenance with per-rule CST node counts;
- grammar diagnostic facts that attach syntax diagnostics to the active
  grammar, activation, effective rule, source span, diagnostic code, and
  structured expected fragment when present;
- syntax diagnostics from the existing parser.

The normal pipeline is unchanged:

```text
GrammarParseReport.syntax -> AST -> semantic IR -> lowered IR -> execution_tape
```

`ast.rs`, `sem.rs`, `lower.rs`, and `tape.rs` do not perform grammar lookup.
They continue to consume the same syntax and semantic products as before.

## Workbench Use

Use `interloko_lab` to inspect the grammar projection:

```sh
cargo run -p interloko_lab -- dump corpus/pass/generics/two_class_dispatch.loko --stage mop
cargo run -p interloko_lab -- eop corpus/pass/generics/two_class_dispatch.loko
cargo run -p interloko_lab -- inspect corpus/pass/generics/two_class_dispatch.loko grammar:base-loko
cargo run -p interloko_lab -- invoke-affordance corpus/pass/generics/two_class_dispatch.loko grammar:base-loko show-grammar-stack
cargo run -p interloko_lab -- invoke-affordance corpus/pass/generics/two_class_dispatch.loko grammar-rule:base-loko/function-definition show-grammar-rule
cargo run -p interloko_lab -- invoke-affordance corpus/pass/generics/two_class_dispatch.loko language-environment:base-loko show-parse-trace
```

Workbench and corpus files can select the derived fixture with a leading
header:

```text
// grammar: derived-host
```

The default is `// grammar: base-loko`. The header is workbench/corpus session
input, not Loko syntax.

The artifact graph now separates the frontend facts:

```text
source -> environment-resolution -> grammar-stack -> grammar-parse -> parse
```

This makes grammar-stack identity explicit without making ordinary method-body
edits look like grammar edits.

`show-parse-trace` prints grammar diagnostic facts without rendering localized
diagnostic prose. Presentation tools remain responsible for turning diagnostic
codes and structured args into user-facing text.

## Source Data

`interloko_compiler::source_data` provides a small inert data model:

```text
symbol
string
integer
boolean
list
tagged record
```

The grammar MOP uses this for rule expression records, for example:

```text
grammar-rule(name: function-definition, expression: "...", location: "syntax.rs:Parser::parse_function")
```

This is deliberately not Loko syntax. It is a structural carrier for frontend
facts so later staged parsing or semantic actions can exchange data-like forms
without running host code or pretending macros already exist.

## Deferred Design

Later slices may add:

- source-authored grammar declarations;
- staged parsing over source regions;
- structural semantic actions that emit forms or grammar deltas.

Those later actions must remain deterministic and inspectable. They should emit
structural results, not run arbitrary host code.
