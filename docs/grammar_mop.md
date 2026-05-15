# Grammar MOP

Interloko exposes the current hand-written Loko parser as read-only grammar
metaobjects. It also has trusted staged grammar adapters that emit structural
frontend forms that are lowered directly into AST definitions.
This is still not a parser rewrite and not a macro system.

## Boundary

`interloko_compiler::grammar_frontend` owns language-environment identity,
grammar metaobjects, grammar-stack resolution, parser invocation, parse
activations, structural staged emissions, and parse provenance.

It explicitly does not own AST lowering, semantic lowering, source-authored
grammars, semantic actions, host code execution, OMeta/Ohm compatibility, or
user-defined parser rules.

## What Is Real Now

The implemented grammar slice has three load-bearing pieces:

- a read-only grammar MOP projection for the base hand-written parser;
- an explicit session-owned grammar registry and grammar-stack selection path;
- trusted Rust-hosted staged adapters that emit structural frontend forms.

Those frontend forms are compiler data, not source text. `ast.rs` consumes
staged function and class forms directly when building AST definitions.
Materialized Loko is still carried for reports and debugging, but it is no
longer the semantic input for staged definitions.

The remaining text-shaped fallback is inside staged function bodies:
`FrontendExpression::LokoText` is parsed by the existing expression parser.
That keeps this slice small while leaving a clear follow-up path toward fully
structural frontend expressions.

## Current Records

The frontend selects language environments and staged region grammars through
`GrammarFrontendRegistry`. The built-in registry exposes these environment
tokens:

```text
base-loko
derived-host
```

Tools use that registry to validate `// grammar:` headers instead of matching
hardcoded strings. `GrammarEnvironmentSelection` is a registry token; callers
can use `GrammarEnvironmentSelection::base_loko()`,
`GrammarEnvironmentSelection::derived_host()`, or
`GrammarFrontendRegistry::builtin().environment_selection(token)`.
Compiler sessions own the registry they compile with, and EOP projection exposes
that session registry as inspectable objects:

```text
grammar-registry:session
registered-grammar-environment:base-loko
registered-grammar-environment:derived-host
staged-grammar-adapter:calculator
staged-grammar-adapter:records
```

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

The staged prototype adds:

```text
language-environment:staged-loko  StagedLokoEnvironment
grammar:calculator                CalculatorRegionGrammar extends BaseLokoGrammar
grammar:records                   RecordsRegionGrammar extends BaseLokoGrammar
```

`CalculatorRegionGrammar` has direct rules for `calculator-region` and
`calculator-function`. A frontend metadata directive selects it by name:
`// grammar-activate: calculator`. Later comment-delimited regions are routed
through the registered calculator adapter and emit structural function forms.

`RecordsRegionGrammar` has direct rules for `records-region`,
`record-definition`, and `record-field`. It is selected with
`// grammar-activate: records` and emits structural class forms with typed
slots. `ast.rs` consumes those structural forms directly as compiler-generated
definitions with source spans from the staged region. Materialized Loko is still
stored on each staged region for dumps, reports, and debugging, but the semantic
pipeline no longer reparses it as the source of truth. Both adapters are trusted
Rust-hosted entries in the same explicit registry protocol; there is no
source-authored grammar definition syntax yet.

Each staged adapter also exposes a `GrammarExtensionDescriptor`. The descriptor
is the readable protocol record for the extension:

```text
adapter: records
grammar: records / RecordsRegionGrammar
activation directive: // grammar-activate: records
region start: // staged-loko: records
region end: // end-staged-loko
accepted region: `record name` blocks with typed fields
capabilities: emit-frontend-forms
emits: frontend-class
provenance: records-region, record-definition, record-field
```

Concrete parses add three more record families:

- extension activations, tying an activation directive to a source region;
- extension emissions, one per frontend form emitted by the adapter;
- extension provenance, one per charged staged grammar rule.

These records are deliberately still read-only. They make trusted Rust-hosted
extensions visible as protocol data without claiming that source-authored
grammars, semantic actions, or a macro system exist yet.

## Parse Reports

The grammar frontend wraps the existing `syntax::parse_file` path and returns a
`GrammarParseReport`.

That report contains:

- the parsed `SyntaxTree`;
- the resolved language environment;
- the resolved grammar stack;
- projected grammar, direct-rule, and effective-rule records;
- grammar activations over source regions;
- staged regions with frontend forms, materialized ordinary Loko debug text,
  and inert source-data parse results;
- staged grammar extension activations, emitted frontend forms, and charged
  provenance rules;
- parse provenance with per-rule CST node counts;
- grammar diagnostic facts that attach syntax diagnostics to the active
  grammar, activation, effective rule, source span, diagnostic code, and
  structured expected fragment when present;
- syntax diagnostics from the existing parser.

The normal pipeline now has one explicit structured-input join point:

```text
GrammarParseReport.syntax + staged frontend forms -> AST -> semantic IR -> lowered IR -> execution_tape
```

`ast.rs` lowers staged `FrontendForm::Function` and `FrontendForm::Class`
values into AST definitions. `FrontendExpression::LokoText` is still a
transitional expression payload and is parsed through the existing expression
parser under the hood. `sem.rs`, `lower.rs`, and `tape.rs` do not perform
grammar lookup and continue to consume ordinary AST/semantic products.

## Workbench Use

Use `interloko_lab` to inspect the grammar projection:

```sh
cargo run -p interloko_lab -- dump corpus/pass/generics/two_class_dispatch.loko --stage mop
cargo run -p interloko_lab -- eop corpus/pass/generics/two_class_dispatch.loko
cargo run -p interloko_lab -- registry corpus/pass/generics/two_class_dispatch.loko
cargo run -p interloko_lab -- extensions tools/interloko_lab/fixtures/staged_records.loko
cargo run -p interloko_lab -- inspect corpus/pass/generics/two_class_dispatch.loko grammar-registry:session
cargo run -p interloko_lab -- invoke-affordance corpus/pass/generics/two_class_dispatch.loko grammar-registry:session show-grammar-registry
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/staged_records.loko grammar-extension:records show-grammar-extension
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

Use the staged calculator adapter like this:

```text
// grammar-activate: calculator

// staged-loko: calculator
helper = 40
// end-staged-loko

// staged-loko: calculator
main = helper() + 2
// end-staged-loko
```

The staged regions emit structural `frontend-function` forms. AST lowering
consumes those function forms directly, while each body is currently carried as
`FrontendExpression::LokoText` and parsed by the existing expression parser.
Reports also show a materialized ordinary-Loko debug projection:

```text
define function helper()
  40
end

define function main()
  helper() + 2
end
```

Inspect it with:

```sh
cargo run -p interloko_lab -- run tools/interloko_lab/fixtures/staged_calculator.loko
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/staged_calculator.loko language-environment:staged-loko show-parse-trace
```

Use the staged records adapter like this:

```text
// grammar-activate: records

// staged-loko: records
record circle
  radius: <integer>
end
// end-staged-loko

define function main()
  let c = make(<circle>, radius: 10);
  radius(c)
end
```

The records region emits a structural `frontend-class` form that AST lowering
consumes directly. Reports also show a materialized ordinary class definition
as a debug projection:

```text
define class <circle> (<object>)
  slot radius :: <integer>, required-init-keyword: radius:;
end
```

Inspect it with:

```sh
cargo run -p interloko_lab -- run tools/interloko_lab/fixtures/staged_records.loko
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/staged_records.loko language-environment:staged-loko show-parse-trace
```

The artifact graph now separates the frontend facts:

```text
source -> environment-resolution -> grammar-stack -> grammar-parse -> parse
```

This makes grammar-stack identity explicit without making ordinary method-body
edits look like grammar edits.

`show-parse-trace` prints grammar diagnostic facts without rendering localized
diagnostic prose. Presentation tools remain responsible for turning diagnostic
codes and structured args into user-facing text.

For staged regions, `show-parse-trace` and EOP records show the adapter name,
activation directive span, source span, frontend forms, debug materialized
source, and inert source-data result. The directive and `// staged-loko:` markers are
frontend/workbench metadata; they are not part of the Loko language semantics.

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

Frontend forms currently project to source data such as:

```text
frontend-function(name: main, ...)
frontend-class(name: <circle>, superclass: <object>, ...)
frontend-slot(name: radius, type: <integer>, ...)
```

These records are the inspectable expander boundary. They are not executable
semantic actions, and they do not bypass the existing parser.

## Deferred Design

Later slices may add:

- source-authored grammar declarations;
- structural semantic actions that emit forms or grammar deltas;
- fully structural frontend expressions with no `LokoText` body fallback;
- macro hygiene and name-introduction rules;
- exact original-source span mapping for emitted forms;
- a public registry/capability policy for choosing staged grammar adapters;

Those later actions must remain deterministic and inspectable. They should emit
structural results, not run arbitrary host code.
