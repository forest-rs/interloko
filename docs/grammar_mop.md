# Grammar MOP

Interloko exposes the current hand-written Loko parser as read-only grammar
metaobjects inside a selected language environment. It also has staged grammar
extensions that emit structural frontend forms lowered directly into AST
definitions. The `records` and `calculator` staged grammars are now parsed by
a tiny interpreted grammar path; base Loko is still parsed by the Rust parser.
This is still not a full parser rewrite and not a macro system.

## Boundary

`interloko_compiler::grammar_frontend` owns language-environment identity,
grammar metaobjects, grammar-stack resolution, parser invocation, parse
activations, structural staged emissions, and parse provenance.

It explicitly does not own AST lowering, semantic lowering, user grammar
package loading, semantic actions, host code execution, OMeta/Ohm
compatibility, or a replacement for the base Loko parser.

## What Is Real Now

The implemented grammar slice has three load-bearing pieces:

- a read-only grammar MOP projection for the base hand-written parser;
- language environments that own resolved grammar stacks, activations, and
  provenance;
- a session-owned host registry used as catalog plumbing for available
  environments and staged extensions;
- trusted staged grammar extensions that emit structural frontend forms;
- source-authored staged grammars, `records` and `calculator`, whose grammar
  source is parsed into `RuleExpression` and interpreted over staged regions,
  with explicit staged-region metadata recording the parser driver and oracle
  status.

Those frontend forms are compiler data, not source text. `ast.rs` consumes
staged function and class forms directly when building AST definitions, and the
calculator adapter emits structural expression nodes for integers, names,
calls, and simple arithmetic. Materialized Loko is still carried for reports
and debugging, but it is no longer the semantic input for staged definitions or
staged function bodies.

## Current Records

The frontend selects language environments and staged region grammars through a
host-side `GrammarFrontendRegistry`, but the registry is not the center of the
model. It is a catalog of providers. The active `LanguageEnvironment` is the
object that owns the grammar stack for a compile.

The built-in registry exposes these environment tokens:

```text
base-loko
derived-host
```

Tools use that registry to validate `// grammar:` headers instead of matching
hardcoded strings. `GrammarEnvironmentSelection` is a registry token; callers
can use `GrammarEnvironmentSelection::base_loko()`,
`GrammarEnvironmentSelection::derived_host()`, or
`GrammarFrontendRegistry::builtin().environment_selection(token)`.
Compiler sessions own the registry they compile with, and EOP projection still
exposes the session registry as inspectable plumbing:

```text
grammar-registry:session
registered-grammar-environment:base-loko
registered-grammar-environment:derived-host
staged-grammar-adapter:calculator
staged-grammar-adapter:records
```

The active projection names the selected language environment and its grammar
stack:

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

Environment and grammar objects expose navigation affordances:

```text
show-grammars         environment or grammar summary
show-direct-rules     direct rules projected by a grammar
show-effective-rules  resolved inherited/overridden rules for a grammar
show-activations      source activations for an environment or grammar
show-grammar-stack    active stack for the compile
show-parse-trace      full parse provenance
show-expression       structured expression for a direct grammar rule
show-parser-shadow    primary/oracle parser shadow report
```

This keeps the Ohm/OMeta-like path visible: start at the active environment,
follow the grammar stack, inspect direct rules versus effective rules, then
follow activations back to source regions and emitted frontend forms. The
current rules are still compiler/host projected, not source-authored.

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

`CalculatorRegionGrammar` is grammar-driven. Its direct rules are loaded from
`crates/interloko_compiler/grammars/calculator.grammar`:

```text
grammar calculator
rule calculator-region = calculator-function*
rule calculator-function = name `=` expression
  => emit-function(name: name, body: expression)
rule expression = term additive-tail*
  => fold-binary(lhs: term, tails: additive-tail*)
rule additive-tail = add-op term => emit-binary-tail(op: add-op, rhs: term)
rule add-op = `+` | `-`
rule term = factor multiplicative-tail*
  => fold-binary(lhs: factor, tails: multiplicative-tail*)
rule multiplicative-tail = `*` factor
  => emit-binary-tail(op: "*", rhs: factor)
rule factor = parenthesized | call | number-literal | name-ref
rule parenthesized = `(` expression `)`
rule number-literal = number => emit-integer(value: number)
rule name-ref = name => emit-name(name: name)
rule call = name `(` argument-list? `)` => emit-call(callee: name, args: argument-list*)
rule argument-list = expression argument-tail*
  => emit-argument-list(first: expression, rest: argument-tail*)
rule argument-tail = `,` expression => emit-argument-tail(value: expression)
rule name = [A-Za-z_] [A-Za-z0-9_?-]*
rule number = [0-9]+
end
```

A frontend metadata directive selects it by name:
`// grammar-activate: calculator`. Later comment-delimited regions are
recognized by the interpreted grammar and converted into structural function
forms through the `emit-function`, expression, call, and left-fold binary
actions. Rust still implements those safe action primitives and
calculator-specific validation, but it is no longer a duplicate recognizer, a
hidden token parser, or a bespoke expression-tree walker.

`RecordsRegionGrammar` is also grammar-driven. Its direct rules are loaded from
`crates/interloko_compiler/grammars/records.grammar`:

```text
grammar records
rule records-region = record-definition*
rule record-definition = `record` record-name record-field* `end`
  => emit-class(name: record-name, superclass: "<object>", slots: record-field*)
rule record-name = type-name | identifier
rule record-field = field-name `:` type-name `;`?
  => emit-slot(name: field-name, type: type-name, init-keyword: field-name,
               required-init-keyword: true)
rule field-name = identifier
rule type-name = `<` identifier `>`
rule identifier = [A-Za-z_] [A-Za-z0-9_?-]*
end
```

It is selected with `// grammar-activate: records`; later
`// staged-loko: records` regions are recognized by the interpreted grammar,
which produces a parse tree with rule names and spans. The `emit-class` and
`emit-slot` actions convert captures from that parse tree into structural class
forms with typed slots.
`ast.rs` consumes those structural forms directly as compiler-generated
definitions with source spans from the staged region. Materialized Loko is still
stored on each staged region for dumps, reports, and debugging, but the semantic
pipeline no longer reparses it as the source of truth. Rust still implements
the safe `emit-class`/`emit-slot` action primitives and records-specific
validation, but it is no longer a duplicate records parser or a bespoke
record-to-form walker.

`CalculatorRegionGrammar` follows the same source-authored/interpreted grammar
path for recognition, while Rust still converts the interpreted expression tree
into structural function forms. Both grammars still live behind the same
explicit registry protocol; there is no user package/loading story for external
grammar files yet.

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

These records are deliberately still read-only. They make staged extensions
visible as protocol data without claiming that semantic actions, arbitrary user
grammar packages, or a macro system exist yet.

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
- parse provenance with individual rule applications and per-rule CST node
  counts;
- grammar diagnostic facts that attach syntax diagnostics to the active
  grammar, activation, effective rule, source span, diagnostic code, and
  structured expected fragment when present;
- syntax diagnostics from the existing parser.

The normal pipeline now has one explicit structured-input join point:

```text
GrammarParseReport.syntax + staged frontend forms -> AST -> semantic IR -> lowered IR -> execution_tape
```

`ast.rs` lowers staged `FrontendForm::Function` and `FrontendForm::Class`
values into AST definitions. Staged function bodies lower from structural
`FrontendExpression` records into AST expressions instead of generated text.
`sem.rs`, `lower.rs`, and `tape.rs` do not perform grammar lookup and continue
to consume ordinary AST/semantic products.

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
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/staged_records.loko language-environment:staged-loko
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/staged_records.loko language-environment:staged-loko show-grammars
cargo run -p interloko_lab -- inspect corpus/pass/generics/two_class_dispatch.loko grammar:base-loko
cargo run -p interloko_lab -- invoke-affordance corpus/pass/generics/two_class_dispatch.loko grammar:base-loko show-direct-rules
cargo run -p interloko_lab -- invoke-affordance corpus/pass/generics/two_class_dispatch.loko grammar:base-loko show-effective-rules
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
consumes those function forms and their structural expression bodies directly.
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

`show-parser-shadow` is the base-Loko self-hosting harness surface. Today it
reports the source-authored base-Loko grammar parser as the primary parser and
compares it with the Rust recursive-descent parser as oracle. The comparison is
still normalized to the top-level class, generic, method, and function
signature, but ordinary compilation now flows through grammar-primary parsing.

Example output:

```text
parser shadow status=equivalent
primary:
  implementation source-authored-base-loko-grammar-primary
  normalized-nodes 5
  top-level-nodes 4
  top-level-signature class-definition,generic-definition,method-definition,function-definition
oracle:
  implementation rust-recursive-descent-oracle
```

If the source-authored shadow grammar disagrees with the Rust parser, the report
switches to `diverged` and lists normalized-node, top-level-count,
top-level-signature, diagnostic, or rule-application differences.

For staged regions, `show-parse-trace` and EOP records show the adapter name,
parser driver, oracle/fallback status, activation directive span, source span,
frontend forms, debug materialized source, and inert source-data result. For
records and calculator, the metadata is explicit:

```text
driver=interpreted-grammar
oracle=none
```

The inert result repeats those facts as `driver: interpreted-grammar` and
`oracle: none`, then includes the `rule-node` parse tree. The directive and
`// staged-loko:` markers are frontend/workbench metadata; they are not part of
the Loko language semantics.

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
grammar-rule(name: function-definition, expression: sequence(...), text: "...", location: "syntax.rs:Parser::parse_function")
```

Direct grammar rules also carry a typed `RuleExpression`. `show-expression`
renders both the readable expression and the inert `SourceData` form so tools
do not have to parse display text to understand the grammar projection.

This is deliberately not Loko syntax. It is a structural carrier for frontend
facts so later staged parsing or semantic actions can exchange data-like forms
without running host code or pretending macros already exist.

Frontend forms currently project to source data such as:

```text
frontend-function(name: main, ...)
frontend-binary(op: +, ...)
frontend-call(callee: helper, ...)
frontend-class(name: <circle>, superclass: <object>, ...)
frontend-slot(name: radius, type: <integer>, ...)
```

These records are the inspectable expander boundary. They are not executable
semantic actions, and they do not bypass the existing parser.

## Deferred Design

Later slices may add:

- user-authored grammar declarations loaded from ordinary source files;
- structural semantic actions that emit forms or grammar deltas;
- wider base-Loko shadow grammar coverage beyond the current top-level subset;
- a complete structural frontend expression model for the full Loko expression
  language;
- macro hygiene and name-introduction rules;
- exact original-source span mapping for emitted forms;
- a public registry/capability policy for choosing staged grammar adapters;

Those later actions must remain deterministic and inspectable. They should emit
structural results, not run arbitrary host code.
