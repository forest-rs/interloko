# Plans

## Complete Scalar Host Functions

### Goal

Make `HostMetaRegistry::add_function` load-bearing: registered integer,
boolean, and string functions resolve in Loko source, lower to ordinary tape
host calls, appear in EOP inspection, and execute through a calm scalar
handler API.

### Fence

Interloko owns declaration, type checking, lowering, scalar value conversion,
and bounded VM dispatch. Embedders own the meaning and lifecycle of each host
function. A host call does not imply authorization, persistence, or mutation.

### Invariants

- Host signatures contain no compiler-local class ids.
- VM handles and runtime heap values do not cross the handler API.
- Missing, rejected, and type-invalid handlers produce structured call errors.
- Existing calls that use no declared host function retain their behavior.
- The compiler and runtime remain `no_std + alloc`.

## Add Packrat Memoization To The Grammar Kernel

### Goal

Make `interloko_grammar` memoize rule applications during one parse so
backtracking-heavy grammars reuse prior work instead of repeatedly matching the
same `(grammar scope, rule, input position)` tuple. Keep the public grammar
semantics unchanged while exposing lightweight parse metrics for inspection.

### Non-Goals

- No Pratt parser or precedence table.
- No tree/value matcher.
- No new production dependencies; keep `interloko_grammar` `no_std`.
- No change to direct left-recursion semantics.
- No persistent cache across parses.

### Fence

`interloko_grammar` owns per-parse rule memoization and parser work metrics. It
explicitly does not own compiler artifact caching, semantic lowering, or
cross-parse scheduling.

### Invariants

- Memoization keys include grammar scope, rule name, and local input position.
- Memoized failures preserve the same expected fragments and cut-boundary
  behavior as direct matching.
- Direct left-recursive rules still use the existing seed/grow algorithm.
- Slice-mode and compiled-mode parsing produce the same parse trees and errors
  as before.
- Metrics are observational only; callers must not need them to get correct
  parses.

### Sequence

1. Introduce a parse context with a small dependency-free memo table and
   `ParseMetrics`.
2. Thread the context through rule/expression interpretation.
3. Memoize concrete rule dispatches, bypassing cache for parametric argument
   substitution while still memoizing concrete inner rules.
4. Surface metrics on inspection/recovery reports and compiler parse artifacts;
   add tests proving reuse.
5. Run fmt, clippy, workspace tests, and docs, then commit.

## Streamline Grammar Frontend Parse Artifacts

### Goal

Remove transitional parse plumbing now that the recovering grammar parser is
load-bearing for base Loko. Make the owned parse tree the single stored parse
artifact, derive generic dump data from it when needed, and collapse the
base-loko primary parser into one output shape.

### Non-Goals

- No grammar semantics changes.
- No change to recovery behavior or diagnostic wording.
- No removal of strict/interpreted APIs used by staged records/calculator.
- No compatibility aliases; callers are internal and may be updated directly.

### Fence

`interloko_grammar` owns matching, recovery, and parse-tree shape. It
explicitly does not own Loko CST projection, compiler diagnostics, EOP records,
or report formatting.

`grammar_frontend` owns converting parser output into CST, diagnostics,
artifacts, and provenance. It explicitly does not duplicate parser report
semantics or store parallel projections of the same parse tree.

### Invariants

- `OwnedRuleNode` is the only stored parse-tree artifact.
- `SourceData` parse-tree dumps are derived from `OwnedRuleNode` on demand.
- Base-loko parsing returns one output object with syntax and optional parse
  tree, not success/failure wrapper types.
- Parse diagnostics remain attached to the syntax tree.
- Staged records/calculator keep their current strict interpreted parse path.

### Sequence

1. Add owned parse-tree `SourceData` projection and remove stored
   `parse_tree_data`.
2. Collapse `BaseLokoParseSuccess` / `BaseLokoParseFailure` into a single
   base-loko parse output.
3. Remove now-unused wrappers/imports and update tests/progress docs.
4. Run fmt, clippy, workspace tests, and docs before committing.

## Add Binding Type Annotations

### Goal

Extend Loko's existing annotation syntax so immutable top-level constants and
local `let` bindings can declare value types:

```loko
define constant $answer :: <integer> = 42;
define function main()
  let radius :: <integer> = 10;
  radius
end;
```

Parameters, method specializers, slot types, and result annotations already use
the same `::` vocabulary. This slice makes value bindings follow that pattern
instead of treating constants and locals as inferred-only special cases.

### Non-Goals

- No mutable top-level variables.
- No general type aliases, unions, parametric types, or refinements.
- No new runtime representation for constants or locals.
- No constant folding, inference scheduler, or flow-sensitive local annotation
  refinement.
- No annotations on arbitrary assignment targets.

### Fence

`grammar_frontend` owns recognizing optional binding annotations and projecting
them into CST. It explicitly does not decide whether the annotation is valid.

`ast` owns preserving the optional `TypeAnnotation` on constants and lets. It
explicitly does not resolve classes or compare value kinds.

`sem` owns resolving annotations and checking initializer/value compatibility.
It explicitly does not introduce new storage or runtime evaluation machinery.

`lower` owns respecting the semantic local kind when emitting a local binding.
It explicitly does not reinterpret annotations or issue diagnostics.

### Invariants

- Existing unannotated constants and lets keep their current behavior.
- Annotated constants and lets use the same type-resolution rules as function
  annotations.
- Mismatches are structured diagnostics with stable codes and MessageFormat
  catalog entries.
- Lowering continues through ordinary expression/local machinery.

### Sequence

1. Extend base-loko grammar/CST projection and AST records.
2. Add semantic annotation checks for constants and lets.
3. Make lowered locals coerce to the semantic local kind.
4. Add unit, driver, corpus, and docs/progress coverage.
5. Run fmt, clippy, workspace tests, docs, then commit cleanly.

## Add Top-Level Constants

### Goal

Add immutable top-level `define constant $name = expression;` definitions to
Loko. Constants should be ordinary named values available from functions,
methods, and other expressions, while remaining inspectable through the same
source-form, EOP/MOP, REPL, lab, and corpus surfaces as classes/generics/
methods/functions.

### Non-Goals

- No mutable top-level variables.
- No modules, imports, exports, or package scope rules.
- No macro system.
- No optimizer or constant-folding pass.
- No compile-time evaluation beyond binding the initializer expression into
  executable code.
- No REPL redefinition protocol beyond rebuilding the accepted source buffer.
- No persistence/image semantics.

### Fence

`syntax` and `grammar_frontend` own recognizing constant definitions and
projecting them into CST. They explicitly do not own semantic binding or
runtime execution.

`ast` owns the constant definition AST node. It explicitly does not resolve or
evaluate constants.

`sem`, `lower`, and `tape` own binding constants and making their values usable
through the existing expression pipeline. They explicitly do not introduce
mutable global state.

`session`, `interloko_repl`, and `interloko_lab` own projection and inspection
of constant objects. They explicitly do not infer constants by parsing dump
text.

### Invariants

- `define constant` creates a stable `DefinitionKind::Constant` id.
- Constants are source-authored immutable definitions with EOP/MOP source and
  artifact facets.
- Constant references use the same expression semantics as other names.
- Constant initializers lower through the ordinary expression/lowered/tape path,
  not through an ad hoc evaluator.
- Duplicate names and unresolved references stay structured diagnostics.
- Existing source and staged grammar behavior keeps working.

### Sequence

1. Add grammar/CST/AST support for `define constant $name = expression;`.
2. Add semantic constant symbols, duplicate checks, and reference resolution.
3. Lower constants through module initialization/existing expression machinery
   enough for functions and methods to read them.
4. Project constants through session/EOP/MOP/source-form tooling and update REPL
   and lab lookups.
5. Add corpus, unit, REPL/lab, and docs/progress coverage.
6. Run fmt, clippy, workspace tests, docs, and commit in coherent slices.

### Risks

- A top-level value can accidentally become hidden global mutable state. Keep
  constants immutable and value-producing only.
- Constant initializers may want dependency ordering. Start with source-order
  constants and report unresolved references rather than adding a scheduler.
- Object constants can pressure runtime ownership if interpreted as persistent
  heap state. Treat them as ordinary executable values in this slice.

## Clarify The Frontend Parse Boundary

### Goal

Make the compiler frontend pipeline explicit: source text is parsed by a
selected grammar environment, that parse produces an inspectable parse-tree
artifact, and a CST projector turns the parse tree into the compiler
`SyntaxTree` consumed by AST lowering.

### Non-Goals

- No LSP server.
- No formatter.
- No syntax-highlighting API yet.
- No crate split.
- No new grammar syntax.
- No AST, semantic IR, lowering, runtime, or tape behavior changes.

### Fence

`interloko_grammar` owns the borrowed parser kernel. It explicitly does not own
compiler CST projection or long-lived session artifacts.

`interloko_compiler::grammar_frontend` owns grammar-environment selection,
frontend parse reports, parse-tree-to-CST projection, grammar provenance, and
compiler diagnostics. It explicitly does not own AST lowering, semantic IR, or
runtime execution.

### Invariants

- The active parser path still produces the existing `SyntaxTree` shape.
- The parse tree is not discarded during CST projection; an owned parse artifact
  is preserved in `GrammarParseReport`.
- Borrowed grammar-kernel parse nodes do not leak into long-lived compiler
  reports.
- Staged-region blanking remains explicit in the frontend artifact.
- Existing session, lab, REPL, corpus, and examples keep their behavior.

### Sequence

1. Rename the base Loko CST builder role around projection rather than parsing.
2. Return a base parse success containing both owned parse-tree source data and
   the produced `SyntaxTree`.
3. Add a `FrontendParseArtifact` to `GrammarParseReport`.
4. Populate it for ordinary and staged parses.
5. Add focused tests and run full validation.

### Risks

- Duplicating parse-tree source data in reports can add memory pressure. Keep it
  as owned structural report data for now, not a second compiler tree API.
- Staged parses use a blanked parser source; the artifact must name that fact so
  future tooling does not confuse it with original source text.
- This slice should expose the boundary, not start a formatter/LSP subsystem.

## Add Syntax Tooling Queries

### Goal

Give future formatter, highlighter, and editor surfaces a calm compiler API for
asking source-oriented syntax questions without reaching into parser internals.
The API should expose parse artifact metadata, CST node summaries, token
summaries, and grammar-rule applications for a byte span.

### Non-Goals

- No formatter implementation.
- No highlighter implementation.
- No LSP server.
- No parser algorithm changes.
- No new source map format.

### Fence

`SessionArtifacts` owns the stable query surface because it already holds the
source, syntax tree, grammar parse report, and artifact provenance. The grammar
kernel still owns parser execution, and syntax tooling queries do not bypass the
ordinary source-to-grammar-to-CST pipeline.

### Invariants

- Query results are owned summaries, not borrowed CST nodes.
- Query methods work for byte ranges and zero-width cursor spans.
- Grammar-rule provenance comes from recorded parser applications, not from
form-kind heuristics.
- Existing dumps and EOP records remain unchanged.

### Sequence

1. Add owned syntax node/token summary types.
2. Expose `SessionArtifacts::frontend_parse_artifact`.
3. Expose span queries for CST nodes, tokens, and grammar-rule applications.
4. Add tests using a function body span and cursor span.

## Structure Grammar Diagnostic Context

### Goal

Make grammar diagnostics explain which recorded parser application supplied
their context. Diagnostics should carry a stable context classification and
the application span where available, so tools can link an error to parse
provenance without slicing parse-trace text.

### Non-Goals

- No new diagnostic renderer.
- No localization changes.
- No recovery algorithm changes.
- No attempt to improve wording in this slice.

### Fence

`grammar_frontend` owns the diagnostic facts because it owns parser
applications and grammar provenance. `session` may render those facts, but it
does not infer parser context from CST shape or diagnostic prose.

### Invariants

- Diagnostic facts are derived from recorded rule applications first.
- Fallback to the root rule stays explicit when no application context exists.
- Existing diagnostic codes and structured args remain unchanged.
- Parse trace output includes the context without becoming the only API.

### Sequence

1. Add a grammar diagnostic context enum and application span field.
2. Resolve diagnostic context from recorded rule applications.
3. Render the new fields in parse-trace and semantic dumps.
4. Add focused tests for context and trace output.

## Borrow Grammar Kernel Strings

### Goal

Make `interloko_grammar` stop allocating strings for grammar-source and
input-source data that can be borrowed. Grammar expressions, actions, parsed
rules, parse trees, parse errors, and rule-count reports should carry lifetimes
that make their relationship to grammar source and parsed source explicit.

### Non-Goals

- No new grammar syntax.
- No parser algorithm changes.
- No package loader or persistent grammar registry.
- No attempt to remove all allocations; recursive expressions and child lists
  still use `Box`/`Vec`.
- No Interloko semantic changes.

### Fence

`interloko_grammar` owns borrowed parser-kernel data tied to grammar source and
parsed input source. It explicitly does not own compiler projection
provenance, semantic action evaluation, source-data rendering, or MOP/EOP
objects.

`interloko_compiler::grammar_frontend` owns any conversion from borrowed grammar
records into compiler-owned projection records that must outlive a local parse.

### Invariants

- Grammar source parsing returns `SourceAuthoredRule<'g>` values borrowing from
  the supplied grammar source.
- Interpreted parse trees return `RuleNode<'g, 's>` values: node kind/rule names
  borrow from grammar source, matched text borrows from parsed source.
- `ParseError` and `RuleCount` stop owning expected/rule-name strings when they
  can borrow grammar fragments.
- Compiler-owned `GrammarRule` records remain owned where session/report data
  needs stable storage beyond the local parse.
- `interloko_grammar` remains `no_std`.
- Workspace tests, clippy, docs, and doctests stay green.

### Sequence

1. Add lifetime parameters to grammar expression/action/rule types and parser
   helpers.
2. Replace parse-tree synthetic `rule_name: String` with a borrowed
   `RuleNodeKind<'g>` plus text slices borrowed from the parsed source.
3. Update parse failures, parse errors, and rule-count reports to borrow
   expected fragments and rule names.
4. Update compiler adapters to convert borrowed grammar records into owned
   compiler records at the projection boundary.
5. Update README/rustdoc examples and run full validation.

### Risks

- Lifetimes can spread into compiler-owned long-lived records. Keep conversion
  to owned data at the compiler boundary.
- Anonymous node labels used to be synthetic strings. Preserve stable display
  labels through methods rather than storing generated strings.
- Public API breakage is intentional here, but docs should make the new
  borrowed ownership model obvious.

## Make Grammar Parse Trees More Inspectable

### Goal

Bring the first Tree-sitter-like usability ideas into `interloko_grammar`
without changing the parser strategy. Parse trees should clearly distinguish
named grammar-rule nodes from anonymous literal/class/opaque nodes, expose
small query helpers, and offer a partial parse report for tools that want to
show the successfully matched tree plus the failure that stopped complete-region
parsing.

### Non-Goals

- No new grammar syntax.
- No LR/GLR parser generator work.
- No source editing or incremental reparsing API.
- No diagnostic rendering/localization.
- No editor integration.
- No Interloko language semantics changes.

### Fence

`interloko_grammar` owns neutral parse-tree roles, parse-tree traversal/query
helpers, and structural partial parse reports. It explicitly does not own
Interloko CST nodes, semantic actions, editor buffers, diagnostics catalogs,
source-data rendering, or incremental scheduling.

`interloko_compiler::grammar_frontend` owns mapping these neutral records into
Interloko source-data reports, staged-region diagnostics, and MOP/EOP
projections.

### Invariants

- Existing callers of `interpret_region` keep their current behavior.
- New APIs are additive and keep `interloko_grammar` `no_std`.
- `RuleNode` remains caller-inspectable without requiring consumers to parse
  `rule_name` prefixes themselves.
- Partial parse reports never pretend a failed complete parse succeeded.
- Query helpers traverse existing parse trees and do not mutate parser state.
- Full workspace validation stays green.

### Sequence

1. Add parse-tree node roles and methods on `RuleNode` for named/anonymous node
   classification.
2. Add simple query/traversal helpers for finding rule nodes, role-filtered
   nodes, and span-covering nodes.
3. Add a partial parse API that returns the matched root when available plus the
   complete-region failure when trailing input or root matching fails.
4. Update compiler source-data rendering to include node roles and use the new
   count/query helpers where appropriate.
5. Update README/rustdoc examples to show the new inspection surface.

### Risks

- Too much query API could turn the kernel into a general tree-processing
  library. Keep helpers small and parse-specific.
- Partial parse reports could be mistaken for recovery. Name and docs must be
  explicit: they are reports, not successful parses.
- Adding public fields to `RuleNode` would break struct-literal callers. Prefer
  methods/enums for this slice.

## Extract The Grammar Kernel

### Goal

Make the grammar system honest as a reusable kernel. Add an `interloko_grammar`
crate that owns grammar rule expressions, grammar action descriptors, grammar
source parsing, interpreted rule matching, parse trees, spans, and parse
failures. Keep Interloko-specific CST building, staged-region policy, MOP/EOP
projection, source-data rendering, and `FrontendForm` emission in
`interloko_compiler`.

### Non-Goals

- No public grammar package loader.
- No new grammar syntax.
- No macro system.
- No new Loko semantics.
- No attempt to publish or stabilize the crate yet.
- No extraction of compiler-specific semantic actions into the grammar crate.

### Fence

`interloko_grammar` owns grammar syntax, neutral rule/action descriptors,
interpreted matching, parse trees, spans, and parse failures. It explicitly does
not own Interloko CST nodes, AST lowering, diagnostics catalogs, staged-region
selection, MOP/EOP projection, or runtime semantics.

`interloko_compiler::grammar_frontend` owns Interloko grammar environments,
base-Loko CST projection, staged-region activation, source-data projection, and
mapping grammar matches to `FrontendForm` values.

### Invariants

- Base Loko, records, and calculator still parse through the same interpreted
  grammar behavior after the split.
- `interloko_grammar` has no dependency on `interloko_compiler`.
- Grammar actions in the new crate are inert data, not host callbacks.
- Source-data rendering remains compiler-owned.
- Workspace tests, corpus, clippy, fmt, and docs stay green.

### Sequence

1. Add `crates/interloko_grammar` and move neutral `Span`,
   `RuleExpression`, `GrammarAction`, grammar-source parsing, and interpreter
   parse tree/failure types into it.
2. Re-export the moved public grammar types from `grammar_frontend` so existing
   compiler/session call sites keep their current path.
3. Leave compiler-owned action evaluation and source-data rendering in
   `grammar_frontend::interpreter`, but make it consume `interloko_grammar`
   parse trees/rules.
4. Move or duplicate the grammar-kernel unit tests into the new crate; keep
   compiler tests focused on Interloko integration.
5. Update docs to describe the crate boundary and run full validation.

### Risks

- Span type duplication can create noisy conversions. Keep conversions explicit
  at the compiler boundary.
- Leaving semantic action evaluation in the compiler may make the first split
  feel partial. That is intentional: action descriptors are reusable data; action
  meanings are compiler-specific.
- Re-exporting types can hide the new crate boundary. Use it only as a migration
  affordance for existing call sites.

## Remove The Rust Recursive-Descent Loko Parser

### Goal

Finish the parser replacement. Base Loko source should parse through the
source-authored grammar path only. The old Rust recursive-descent parser stops
being an oracle, fallback, public mode, or test target, and `syntax` becomes the
lexer/CST substrate that grammar parsing emits into.

### Non-Goals

- No new Loko language semantics.
- No macro system or source-authored grammar package loader.
- No staged grammar redesign.
- No attempt to improve diagnostics beyond preserving existing corpus behavior.
- No removal of lexer, tokens, CST node kinds, spans, or `SyntaxTree`.

### Fence

`syntax` owns lexing, tokens, CST node identity, spans, and `SyntaxTree`
storage. It explicitly does not own parsing base Loko source.

`grammar_frontend::base_loko_primary` owns base Loko parsing into the existing
CST shape. It explicitly does not own AST, semantic IR, lowering, or execution.

### Invariants

- Normal compilation has one base-Loko parser path: the grammar parser.
- No public `ParserMode`, `RustPrimary`, `parse_file_rust`, or Rust-oracle
  fallback remains.
- Invalid-source diagnostics still come from structured diagnostic facts and
  corpus expectations continue to pass.
- Staged records/calculator parsing keeps using the same grammar frontend
  activation model.
- The existing AST/sem/lower/tape pipeline still receives `SyntaxTree`.
- Every commit is bisectable and the workspace stays green.

### Sequence

1. Replace parser-mode/oracle APIs with a single grammar parse path and update
   parser-shadow reporting to describe the active grammar parser without a Rust
   oracle.
2. Move or rewrite the remaining parser tests so they exercise grammar parsing,
   then delete the recursive-descent `Parser` implementation from `syntax.rs`.
3. Tighten grammar-parser diagnostics only where removing fallback exposes a
   corpus or fixture regression.
4. Update docs/tests/report expectations that still talk about Rust-primary or
   recursive-descent oracle mode.
5. Run fmt, clippy, workspace tests, corpus, docs, then commit cleanly.

### Risks

- Invalid syntax behavior may have depended on fallback diagnostics. Fix only
  observed regressions with structured diagnostics in the grammar parser.
- Parser-shadow output becomes less dramatic without an oracle. Keep it useful
  by reporting the parser product summary for the grammar parser.
- Deleting the parser can accidentally delete CST helpers. Keep `CstNode`,
  `CstElement`, `CstKind`, lexer tests, and token APIs intact.

## Flip Base Loko To Grammar-Primary Behind A Parser Mode

### Goal

Stop inching toward a parser replacement and make the source-authored base-Loko
grammar the primary parser path behind an explicit parser mode. The grammar
path must produce the existing `SyntaxTree`/`CstNode` shape that AST lowering
already consumes. The Rust recursive-descent parser remains in tree as an
oracle and emergency fallback while equivalence is proven across the corpus.

This goal is complete when normal session parsing can select
`GrammarPrimary`, valid corpus/examples compile through that mode, parser
shadow compares grammar-primary output against Rust-oracle output, and the
default mode can be flipped without changing downstream AST/semantic/lowering
code.

### Non-Goals

- No removal of the Rust parser in this goal.
- No new language semantics.
- No macro system.
- No new grammar package loading format beyond repo resources.
- No attempt to outperform the hand-written parser.
- No diagnostic-system rewrite beyond preserving existing structured
  diagnostic codes where practical.

### Fence

`syntax` owns lexer tokens, CST node construction, parser modes, and
Rust-oracle fallback. It explicitly does not own grammar MOP projection or
semantic lowering.

`grammar_frontend` owns grammar resources, parser-shadow reporting, and
language-environment selection. It explicitly does not synthesize AST facts by
bypassing `SyntaxTree`.

The base-Loko grammar resource owns the grammar spelling for the current
language surface. It explicitly does not own runtime semantics.

### Invariants

- `SyntaxTree` is still the handoff to AST lowering.
- Grammar-primary parsing constructs CST nodes from grammar-recognized spans
  and token indices, not by returning the Rust parser tree.
- Rust parser remains callable and is used as oracle comparison.
- If grammar-primary cannot build a valid tree, the failure is visible in
  parser-shadow output and tests; fallback is explicit, not silent.
- Parser mode selection is explicit in tests and session plumbing before any
  default flip.
- Existing source files keep compiling with no downstream AST/sem/lower/tape
  changes.
- Every commit is bisectable.

### Sequence

1. Add `ParserMode` and thread it through `syntax` and `grammar_frontend` while
   keeping the default Rust-primary behavior unchanged.
2. Author a full `base_loko.grammar` resource for the current supported syntax:
   definitions, class slots/options, parameter lists, bodies, lets, handlers,
   ifs, assignment, comparisons, arithmetic, calls, slot access, vectors, and
   literals.
3. Build a grammar-primary CST constructor that walks interpreted rule nodes,
   maps spans back to lexer token indices, and emits the existing `CstKind`
   shape.
4. Add equivalence tests comparing grammar-primary and Rust parser products on
   syntax fixtures, corpus programs, staged programs, and representative error
   cases.
5. Route session/lab/repl parsing through parser mode configuration and make
   parser-shadow compare active primary against the oracle.
6. Flip the default parser mode to grammar-primary only after corpus, examples,
   clippy, tests, and docs pass.

### Risks

- The grammar interpreter may need more structure than staged grammars needed.
  Add only primitives required by current Loko syntax.
- Diagnostics are the largest replacement risk. Prefer preserving current
  structured diagnostic codes over inventing new prose.
- CST construction can accidentally depend on presentation strings. Build from
  rule names, spans, and lexer tokens.
- Parenthesized expressions currently disappear as CST nodes. Preserve that
  downstream shape unless AST lowering changes deliberately.

## Build A Real Base-Loko Shadow Grammar Subset

### Goal

Make parser shadow mode useful for base Loko. The Rust recursive-descent parser
stays primary, but a source-authored `base-loko-shadow` grammar should
independently recognize the current top-level definition surface and compare
its normalized parser product against the Rust parser.

This is not the parser flip. It is the first load-bearing base-Loko oracle:
class, generic, method, and function forms are recognized from grammar source,
reported through `show-parser-shadow`, and exercised by tests.

### Non-Goals

- No replacement of `interloko_compiler::syntax`.
- No full expression/body grammar.
- No source-authored semantic actions for base Loko.
- No macro system or new language surface.
- No diagnostic parity claim beyond the normalized shadow subset.
- No performance work.

### Fence

`grammar_frontend::interpreter` owns the small PEG-style matching primitives
needed by source-authored grammars. It explicitly does not own AST, semantic IR,
or executable behavior.

`grammar_frontend` owns the base-Loko shadow grammar resource, normalized parser
product comparison, and parser-shadow reporting. It explicitly does not route
normal compilation through the shadow parser.

### Invariants

- Rust parsing remains primary and trusted.
- The shadow grammar is source-authored grammar text, not projected Rust labels.
- Shadow comparison is normalized to the top-level subset the shadow grammar
  actually recognizes.
- Divergence reports are explicit and diffable.
- Opaque method/function/class bodies are recognized by an explicit grammar
  primitive, not by hidden Rust fallback.
- Existing staged records/calculator grammar paths keep working.
- Every commit is bisectable.

### Sequence

1. Add this plan and keep the boundary explicit.
2. Add a small `until(literal)` rule-expression primitive for opaque delimited
   regions, with structural display/source-data/interpreter support and tests.
3. Add `grammars/base_loko_shadow.grammar` for top-level class, generic,
   method, and function forms.
4. Run the shadow grammar from `parser_shadow_report`, compare normalized
   products against Rust top-level parser facts, and report equivalence or
   divergence.
5. Update REPL/lab/session tests and grammar docs to show the real oracle path.
6. Run fmt, clippy, tests, docs, then commit clean slices.

### Risks

- The normalized summary could hide real parse-tree differences. Name it as a
  top-level shadow subset and keep the full Rust parser as primary.
- `until(literal)` can become a crutch. It is acceptable here only to skip
  bodies while the base expression grammar remains Rust-owned.
- Divergence output can become too coarse. If the subset starts failing in
  confusing ways, add span/rule-level diffs before expanding coverage.

## Harden Interpreted Staged Grammars

### Goal

Make the current staged grammar path obviously load-bearing. Records and
calculator should read as source-authored/interpreted grammars from grammar
source through report output, with no hidden lexical builtins or implied legacy
fallback. The remaining Rust should be narrow adapter machinery: safe action
evaluation, semantic validation, frontend-form construction, and workbench
presentation.

### Non-Goals

- No base-Loko parser flip.
- No external/user grammar package loading.
- No macro system.
- No arbitrary host callbacks from grammar source.
- No parser-theory expansion beyond the existing staged grammar surface.
- No new records/calculator user syntax.

### Fence

`grammar_frontend::interpreter` owns interpreted staged grammar recognition,
parse-tree construction, and safe structural action evaluation. It explicitly
does not own base-Loko parsing, AST lowering, semantic lowering, or arbitrary
host execution.

`grammar_frontend::records` and `grammar_frontend::calculator` own their grammar
resources and staged-language validation. They explicitly do not own duplicate
recognizers or legacy fallback parsers.

Workbench/report surfaces own explanation and linking. They explicitly do not
infer compiler facts by parsing prose dumps.

### Invariants

- Valid records and calculator fixtures keep producing the same frontend forms
  and runtime behavior.
- Calculator token shapes are source-authored grammar rules, not interpreter
  builtins.
- The interpreter reports unknown rule references as grammar failures rather
  than silently matching hidden Rust token parsers.
- Staged regions expose parser driver/oracle facts so users can tell whether a
  region was interpreted and whether a legacy fallback participated.
- Reports and docs make the interpreted path visible without overclaiming a
  base-Loko self-hosting flip.

### Sequence

1. Move calculator lexical rules (`name`, `number`) into
   `calculator.grammar`, remove the remaining staged interpreter builtins, and
   test unknown-rule behavior.
2. Reduce staged adapter glue by parsing each grammar resource once per region
   and sharing action diagnostic conversion between records and calculator.
3. Add explicit staged parser metadata (`driver=interpreted-grammar`,
   `oracle=none`) to `StagedRegionEmission`/`StagedParseRegion`, parse traces,
   EOP records, and the HTML report.
4. Update grammar docs and report tests to show the now-real path:
   grammar resource -> direct rule -> action -> frontend form -> semantic
   object -> artifacts.
5. Run full workspace gates and commit each slice coherently.

### Risks

- Removing builtins may expose grammar-resource omissions. Keep tests around
  calculator calls, arithmetic, records type names, and malformed staged input.
- Parser metadata can overpromise. Use precise values: interpreted staged
  grammars have no legacy oracle/fallback; base-Loko shadow is a separate
  top-level oracle, not staged-region fallback metadata.
- Shared adapter helpers should not hide records/calculator validation. Keep
  semantic checks local to the staged grammar module that owns them.

## Make Grammar Actions Inspectable And Presentable

### Goal

Make staged grammar actions understandable as first-class compiler facts. A user
should be able to start from a staged source region and follow the chain through
grammar package, rule, action, captures, action output, emitted frontend form,
and downstream AST/semantic object without reading raw source-data dumps.

This slice also removes the remaining staged-language line-scanning diagnostic
helpers once shared interpreter/action diagnostics carry enough structure.

### Non-Goals

- No base-Loko parser flip.
- No source-authored grammar package loading from arbitrary project files.
- No new staged language semantics.
- No arbitrary host callbacks from grammar actions.
- No persistent grammar package manager.
- No report UI framework rewrite.

### Fence

`grammar_frontend::interpreter` owns action failure facts, capture facts, and
action output data. It explicitly does not render localized diagnostics or own
HTML/report presentation.

`grammar_frontend` owns grammar package/action metaobjects and affordance names.
It explicitly does not execute actions during inspection.

`interloko_lab` owns report links and visual explanation. It explicitly does not
invent new compiler facts by parsing prose dumps.

### Invariants

- Records and calculator still compile valid staged regions to the same
  frontend forms.
- Malformed staged regions report useful source spans without line-scanning
  helper parsers.
- Grammar packages for `records` and `calculator` have stable object IDs and
  point at checked-in resource paths.
- Grammar actions are visible as structured records with captures and output.
- Existing grammar/rule/package reports stay deterministic and testable.

### Sequence

1. Add structured action diagnostic facts: action, arg, capture rule, and
   expected structural value.
2. Replace `record_shape_diagnostic` and `calculator_shape_diagnostic` with
   interpreter/action diagnostics.
3. Project grammar package and grammar action records into MOP/EOP.
4. Add inspectable affordances for grammar actions: show-captures and
   show-action-output.
5. Link HTML report sections from staged source to grammar resource, rules,
   actions, action outputs, emitted frontend forms, and resulting objects.
6. Update docs/tests and run the full workspace checks.

### Risks

- Action diagnostics can become too generic if they only say "expected action
  output". Include action/arg/rule names in structured args.
- First-class action objects can overpromise mutability. Keep them read-only.
- HTML links can become noisy. Prefer one clear provenance path over trying to
  visualize every rule node.

## Build Structural Grammar Actions And Packages

### Goal

Make staged grammars emit structural frontend forms through declared grammar
actions instead of bespoke Rust control flow. Records and calculator should
recognize source with interpreted grammar rules, bind captures into a shared
action-value model, evaluate safe structural actions, and hand the existing AST
pipeline frontend forms that carry source spans and action provenance.

The useful end state for this slice: records and calculator still behave the
same for users, but their grammar source explains both recognition and emission.
The remaining Rust code should be generic grammar infrastructure, resource
loading, validation helpers, or narrow host-provided emit primitives.

### Non-Goals

- No base-Loko parser flip.
- No arbitrary host callbacks from grammar source.
- No user package manager or untrusted grammar loading policy.
- No left recursion, predicates, parameterized rules, captures beyond the
  action needs, or precedence engine rewrite.
- No optimizer or performance pass.
- No full diagnostic localization/catalog redesign.

### Fence

`grammar_frontend::interpreter` owns recognition, captures, parse trees,
action-value construction, and structural action evaluation. It explicitly does
not own AST, semantic IR, runtime layout, or arbitrary host execution.

Staged grammar modules own grammar resources and any staged-language semantic
validation that cannot honestly be expressed as structural grammar actions yet.
They explicitly do not own duplicate parser paths.

Session/workbench surfaces own inspection, parse traces, reports, and artifact
projection. They explicitly do not re-evaluate grammar actions.

### Invariants

- Records and calculator valid programs keep producing the same frontend forms.
- Parse traces expose action inputs, action outputs, and emitted frontend forms.
- Actions are deterministic structural data transforms, not Rust callbacks.
- Action emit targets are explicit and small: class, slot, function, and
  expression forms needed by records/calculator.
- Grammar source moves out of embedded Rust strings into loadable repo resources
  with stable package/provenance records.
- Records/calculator hand-written shape diagnostics are removed only when the
  shared interpreter/action diagnostics can preserve useful failure locations.
- Base-Loko shadow work is allowed only as a stretch after records/calculator
  no longer need bespoke conversion control flow.

### Sequence

1. Add `GrammarAction`, captures, and `GrammarActionValue` records.
2. Parse `=>` action syntax into structured action data instead of inert
   symbols, preserving existing rule-expression display.
3. Evaluate records actions for classes and slots, then remove records-specific
   parse-tree conversion loops that the action evaluator replaces.
4. Evaluate calculator actions for function and expression forms, keeping
   precedence and validation explicit.
5. Thread action output into staged emission source data, parse traces, MOP/EOP,
   and HTML reports.
6. Move records/calculator grammar source into resource files and load them
   through the grammar registry/session path with stable package provenance.
7. Retire records/calculator shape diagnostics that duplicate shared
   interpreter/action diagnostics.
8. Stretch: add a small base-Loko shadow grammar subset for class, generic, and
   function headers, with Rust parser remaining primary and oracle comparison
   still report-only.

### Risks

- If actions become opaque host callbacks, the grammar MOP becomes decoration
  again. Keep actions data-like and inspectable.
- If captures are too weak, staged grammars will keep bespoke Rust adapters.
  Add only the capture shapes records/calculator need, then stop.
- If diagnostics regress, do not remove the staged-language helper yet.
- Resource loading can sprawl into package management. Keep this slice to
  checked-in grammar resources owned by the compiler crate.
- Base-Loko shadowing can consume the whole goal. It is a stretch item, not a
  blocker for records/calculator action completion.

## Make Records Grammar Lexical Rules Interpreted

### Goal

Make the records staged grammar more PEG-like by moving record names, field
names, and type names out of Rust builtins and into source-authored grammar
rules. The records parser should still emit the same structural class forms,
but the recognizable token shapes should be inspectable grammar expressions.

### Non-Goals

- No base Loko parser flip.
- No external grammar package loading.
- No generalized lexer mode, predicates, captures, or semantic actions.
- No removal of Rust parse-tree to frontend-form conversion.
- No diagnostic rewrite.

### Fence

`grammar_frontend::interpreter` owns small generic PEG primitives such as
character classes and one-or-more repetition. `grammar_frontend::records` owns
the records grammar source and conversion from interpreted rule nodes into
frontend class forms. Neither owns new class semantics or arbitrary grammar
actions.

### Invariants

- Existing valid records staged regions keep producing the same class and slot
  forms.
- Malformed records diagnostics remain useful.
- Records direct rules expose lexical grammar rules for names and type names.
- Calculator can continue using the older tiny builtins until it moves its
  lexical rules over in a separate slice.

### Sequence

1. Add character-class and one-or-more `RuleExpression` primitives plus
   interpreter support.
2. Rewrite the records grammar source to define record-name, field-name,
   type-name, and identifier rules.
3. Keep records-specific validation in Rust conversion code for duplicate
   fields and semantically invalid type names.
4. Update tests and grammar MOP docs to show the source-authored lexical rules.

### Risks

- Named lexical subrules currently skip trivia on entry. Keep directly repeated
  character classes inline until the interpreter grows an explicit lexical
  mode.
- Character classes are intentionally ASCII-only for this slice; broader
  identifier policy should be designed before changing user-facing syntax.

## Retire Staged Grammar Legacy Parsers

### Goal

Make interpreted staged grammars load-bearing enough to remove duplicated
legacy parser paths. The interpreter should return structured failures with
spans and expected fragments, and staged adapters should use those failures
directly instead of falling back to hand-written parsers whenever recognition
fails.

### Non-Goals

- No base Loko parser flip.
- No source-authored semantic actions.
- No full diagnostic-system rewrite.
- No parser-theory expansion beyond the current staged grammar surface.
- No removal of Rust adapter code that converts parse trees into frontend
  forms.

### Fence

`grammar_frontend::interpreter` owns recognition failures: furthest position,
expected fragments, and interpreted parse trees. It explicitly does not own
records/calculator semantic validation or frontend form construction.

The records and calculator modules own staged-language validation and parse
tree to frontend-form conversion. They explicitly do not own generic grammar
failure tracking.

### Invariants

- Records no longer has a duplicate line parser.
- Calculator no longer has a duplicate expression parser.
- Interpreted parse failures become structured diagnostics with source spans.
- Existing valid staged records/calculator programs keep producing the same
  structural frontend forms.
- Malformed staged records/calculator tests continue to produce useful
  diagnostics without legacy fallback.

### Sequence

1. Add `InterpretedParseError` and `interpret_region_result`.
2. Move records to interpreter-only recognition and delete records legacy code.
3. Move calculator to interpreter-only recognition and delete calculator legacy
   code.
4. Update docs/tests to identify the remaining Rust adapter code as semantic
   action/conversion, not parsing.

## Split Grammar Frontend And Interpret Calculator

### Goal

Make the staged grammar work easier to extend by splitting the oversized
grammar frontend into a facade plus focused staged-grammar modules, then move
the calculator staged grammar onto the same source-authored/interpreted path as
records.

The useful result should be concrete: both `records` and `calculator` staged
regions are recognized by source-authored grammar rules, while their legacy
Rust parsers remain available as diagnostic and compatibility backstops.

### Non-Goals

- No base Loko parser flip.
- No removal of the hand-written records or calculator parsers.
- No new grammar package loading story.
- No new user-facing staged language semantics.
- No parser-theory expansion beyond what calculator currently needs.
- No diagnostic rewrite.

### Fence

`grammar_frontend.rs` remains the public facade, registry, environment
resolver, and staged-region orchestrator. It explicitly does not own
grammar-specific parsing details.

`grammar_frontend::interpreter` owns grammar-source loading and interpreted
`RuleExpression` matching. It explicitly does not own frontend form conversion
for any staged language.

`grammar_frontend::records` and `grammar_frontend::calculator` own conversion
from interpreted parse trees into existing structural frontend forms. They
explicitly do not own new language semantics or diagnostics beyond their staged
grammar adapters.

### Invariants

- The first split commit is behavior-preserving.
- Records remains interpreted first, legacy fallback second.
- Calculator becomes interpreted first, legacy fallback second.
- Legacy fallback keeps existing diagnostic quality when the interpreted path
  cannot explain malformed input yet.
- Projected direct rules for source-authored staged grammars carry
  `SourceAuthored` origin and source spans.
- Every commit remains bisectable.

### Sequence

1. Split interpreter and records code into focused submodules with no behavior
   change.
2. Add calculator grammar source and interpreted parse-tree conversion.
3. Route calculator through the interpreter, preserving the legacy parser as a
   fallback.
4. Update tests, docs, and review-note comments around parser fallbacks and
   builtin rules.

### Risks

- Moving code can accidentally change visibility or test-only access. Keep the
  facade API stable and use `pub(super)` only where the parent needs a helper.
- Calculator expressions need operator tokens in the interpreted tree. Keep
  literal nodes inert and avoid making them public grammar rules.
- Diagnostics can regress if the interpreted calculator path accepts partial
  input. Fall back to the legacy parser unless the interpreted path consumes the
  whole staged region and converts cleanly.

## Make The Records Grammar Actually Run

### Goal

Turn one grammar projection into a real parser path. The `records` staged
grammar should be authored as grammar source, parsed by a tiny bootstrap
grammar loader, interpreted over `// staged-loko: records` regions, and used as
the primary recognizer for records syntax. The existing Rust records parser
stays as oracle/projection backstop until the interpreted parse tree proves it
can drive all current records behavior.

This is the first "use, not decoration" grammar slice. It does not self-host
base Loko. It makes a small, useful staged language actually grammar-driven.

### Non-Goals

- No `base-loko` parser flip.
- No removal of `interloko_compiler::syntax`.
- No general macro system.
- No arbitrary semantic actions or host callbacks from grammar source.
- No left recursion, predicates, parameterized rules, captures, or precedence.
- No user package/loading story for grammar files.
- No diagnostic-system rewrite.

### Fence

`interloko_compiler::grammar_frontend` owns the tiny bootstrap grammar loader,
interpreted `RuleExpression` matching, source-authored staged grammar records,
parse-tree provenance, and the records staged grammar handoff. It explicitly
does not own AST/semantic lowering, host callback execution, persistent grammar
packages, or the decision to replace the Rust Loko parser.

The records adapter owns conversion from an interpreted parse tree into
existing structural `FrontendForm::Class` values. It explicitly does not own
new class semantics or a new record language beyond the current fixture syntax.

### Invariants

- `records` staged regions are accepted/rejected by the interpreted grammar,
  not by the legacy line parser.
- The grammar source is parsed into `RuleExpression`; projected direct rules
  come from that source.
- Grammar interpretation returns a structural parse tree with spans and rule
  names.
- Existing records frontend forms remain structural data, not materialized Loko.
- The Rust records parser remains available as an oracle until a later cleanup.
- Diagnostics remain at least as specific as current records diagnostics for
  malformed record headers, malformed fields, duplicate fields, invalid names,
  invalid field types, missing `end`, and empty regions.
- Every commit is bisectable with `records` examples/tests passing.

### Commit Sequence

1. Bootstrap grammar source:
   - add a tiny grammar-source parser for `grammar`, `rule`, literals, rule
     refs, sequence, choice, `?`, `*`, and `=>` output tags;
   - author the `records` grammar as source text;
   - project `records` direct rules from parsed grammar source with source
     spans and `SourceAuthored` origin.
2. Interpreted recognizer:
   - add a small PEG-style interpreter over `RuleExpression`;
   - produce parse tree nodes with rule names, spans, and matched text;
   - keep the interpreter deterministic and backtracking-limited by the current
     grammar surface.
3. Records handoff:
   - make the records adapter call the interpreter first;
   - convert the interpreted tree into `FrontendForm::Class`;
   - keep the legacy Rust parser as oracle/comparison and fallback only for
     diagnostics the interpreted tree cannot yet explain.
4. Provenance and inspection:
   - charge staged extension provenance from interpreted parse tree rule nodes;
   - expose source-authored grammar origin, expression, and parse-tree summary
     in MOP/EOP/show-expression/show-parse-trace.
5. Tests/docs:
   - preserve all current records tests;
   - add tests proving a grammar source edit changes the projected rules;
   - add one direct interpreter unit test for an imaginary small grammar;
   - update grammar MOP docs to say what is now genuinely grammar-driven.

### Risks

- A tiny PEG interpreter can grow sideways into parser theory work. Keep it just
  powerful enough for records and small future staged grammars.
- Diagnostics can regress if recognition and validation get conflated. Keep
  semantic validation for duplicate fields and valid type names outside the
  grammar interpreter.
- "Source-authored" can overpromise if the source is embedded. Wording must be
  honest: this slice parses grammar source text but does not yet load user
  grammar packages from ordinary project files.
- The Rust oracle can become a crutch. Use it for comparison/diagnostic safety,
  not as the primary acceptance path.

## Build The Self-Hosting Grammar Spine

### Goal

Move Interloko toward "Loko grammar as data" without flipping away from the
trusted Rust parser yet. The immediate slice makes grammar rules structurally
inspectable, records span-to-rule applications from the Rust parser, removes
REPL grammar-rule heuristics, and creates the first shadow-equivalence harness
that a future source-authored `base-loko` grammar can plug into.

This is deliberately bolder than a presentation cleanup: after this slice,
the report and REPL should be able to answer "which rule produced this source
span?" from parse facts, and the compiler should have a named place where a
source-authored grammar interpreter can be checked against the Rust parser.

### Non-Goals

- No interpreter primary flip.
- No removal of `interloko_compiler::syntax`.
- No source-authored full `base-loko` grammar in this slice.
- No parser-theory feature expansion: no parameterized rules, left-recursion,
  semantic predicates, or host callbacks.
- No diagnostic-system rewrite.
- No text-emitting grammar actions.
- No performance work beyond keeping the harness cheap enough for tests.

### Fence

`interloko_compiler::grammar_frontend` owns structured grammar rule data,
rule-expression reflection, parser rule applications, and the future
interpreter/oracle boundary. It explicitly does not own AST/semantic lowering,
diagnostic rendering, source-authored grammar package loading, or the policy
decision to remove the Rust parser.

The session, REPL, lab, and corpus layers own presentation, command surfaces,
equivalence reporting, and user-facing demos over those facts. They explicitly
do not invent grammar semantics or re-parse source independently.

### Invariants

- The Rust parser remains primary and trusted throughout this goal.
- Rule expressions are structural data first; string dumps are projections.
- Span-to-rule provenance records actual parser/CST spans, not inferred
  top-level form kinds.
- REPL form grammar lookup uses provenance facts, with heuristics removed.
- Shadow equivalence compares normalized parser products and reports
  diffable divergence; it is allowed to be "no source-authored grammar yet"
  until the interpreter plugs in.
- Grammar actions, when added later, produce structural frontend forms only.
- Every commit is bisectable with the Rust parser as primary.

### Commit Sequence

1. Structured rule reflection:
   - add `RuleExpression` and projection-origin types;
   - replace grammar-rule expression strings as the canonical data;
   - expose `show-expression`/MOP dumps from structured expressions.
2. Span-to-rule provenance:
   - record rule applications `(activation, grammar, rule, span, rule-name)`;
   - include them in parse traces and object dumps;
   - keep count summaries derived from applications.
3. Consumer migration:
   - remove REPL `TopLevelFormKind -> rule-name` lookup;
   - resolve form grammar context by contained rule applications;
   - update report links where a staged/source span can point at a rule.
4. Shadow harness scaffold:
   - introduce a parser-shadow report type with primary/oracle roles;
   - compare Rust parser output against the available projected grammar path;
   - leave interpreter-plug-in status explicit rather than pretending it exists.
5. Tests/docs:
   - unit tests for structured rule expressions and provenance spans;
   - REPL/lab tests showing form -> grammar rule traceback;
   - grammar MOP docs explaining the self-hosting path and oracle fence.

### Risks

- Expression structure can become parser theory cosplay. Keep the first
  variants only as rich as existing projected rules require.
- Provenance can still be too coarse for expressions if it only mirrors CST
  node kinds. Make the coarseness visible and improve it incrementally.
- Shadow mode can look fake before the interpreter lands. Name the missing
  interpreter explicitly and make the harness useful as soon as a source
  grammar is supplied.
- Diagnostics remain the flip gate. This slice should not claim the interpreter
  is ready to replace the Rust parser.

## Make Grammar Environments the Visible Frontend Center

### Goal

Shift the staged frontend presentation from "session registry plus adapters" to
the OMeta/OhmJS-shaped model we want: language environments scope grammars,
grammars inherit and override rules, activations parse source regions, and
trusted host adapters are only the implementation behind grammar objects.

### Non-Goals

- No source-authored grammar syntax.
- No arbitrary host semantic actions.
- No parser rewrite.
- No object-stream grammars.
- No left-recursion controls.
- No grammar package persistence.
- No new language semantics beyond existing structural staged forms.

### Fence

`interloko_compiler::grammar_frontend` owns language environments, grammar
metaobjects, rule inheritance/effectivity, activation provenance, and trusted
structural staged emissions; it explicitly does not own AST/semantic lowering,
source-authored grammars, macro expansion, or host action execution.

The session/workbench layers own presentation, inspection, affordances, and
reports over those facts; they explicitly do not reinterpret grammar lookup or
invent alternate parser semantics.

### Invariants

- The registry remains as host/catalog plumbing, but user-facing inspection
  starts from language environments and grammars.
- A language environment reports its available grammars, active stack, staged
  extension policy, and activation/provenance namespace.
- Staged grammars appear as grammar objects with rules and inheritance, not as
  adapter-shaped primary objects.
- Direct rules, inherited rules, overridden rules, and effective rules remain
  distinguishable.
- Activations say which environment and grammar parsed a region, which root
  rule was used, and which structural forms/objects came from it.
- Registry and adapter records remain inspectable for debugging, but visually
  and textually secondary.

### Commit Sequence

1. Session projection/model:
   - add environment-centered grammar summaries and object affordances;
   - make grammar/staged grammar records describe parent chain, direct rules,
     effective rules, activation policy, and extensions;
   - update object-scoped MOP dumps.
2. REPL surface:
   - name-aware inspection for grammar/environment names;
   - environment/grammar affordances for direct rules, effective rules,
     activations, grammar stack, and parse trace.
3. Report surface:
   - navigation from environment -> grammars -> rules -> activations -> forms
     -> semantic objects;
   - staged regions link to active grammar and effective rules;
   - registry panels remain secondary.
4. Tests/docs:
   - base and staged grammar/environment inspection;
   - records/calculator grammar objects;
   - direct/effective rule display;
   - activation provenance to emitted objects;
   - docs explaining the OMeta/OhmJS shape.

### Risks

- The visible model can overpromise source-authored grammars. Keep wording
  honest: host-backed staged grammars are trusted structural adapters today.
- The registry may leak back into views because it is the current implementation
  spine. Keep registry commands available but secondary.
- Rule-name heuristics can look like real parse traces. Where span-to-rule
  provenance is not implemented, label it as projected or heuristic.

## Rewind The In-Memory Mutation Log

### Goal

Prove the mutation log is operational, not just inspectable, by adding the
first non-append operation on it: rewind. A new
`SessionArtifacts::rewind_to(target: Option<u32>)` truncates the live
session's mutation log at `target` (or back to pristine when `None`),
rebuilds session state from the pristine source plus the retained log
through the same candidate-and-swap discipline used by `apply_mutation`,
and returns a `RewindOutcome` describing what was dropped.

Rewind is not a `SessionMutation`. The mutation log stays a forward
transcript of domain mutations; rewind is an operation *on* that
transcript and is never appended to it. This keeps the invariant
"replaying the log against the base source reproduces current state"
intact: a transcript that included rewind entries would require entries
that erase prior entries, breaking the log's meaning.

### Fence

`SessionArtifacts` owns the pristine source, the log truncation
operation, the rebuild-from-log helper, the candidate-and-swap rollback
for rewind, and the `RewindOutcome` / `RewindError` shape. The new
helper is the load-bearing piece; later replay-against-different-base
and branch slices reuse it. This slice explicitly does not own:

- exposing rewind as a `SessionMutation`, appending rewind entries to
  the log, or any model where the log is anything other than a
  forward transcript of domain mutations;
- public replay against a changed base source file (the helper exists
  internally, but the public API stays scoped to rewind on the
  session's own pristine source);
- branching: forking a session from a prefix of the log into a
  separate session identity;
- log persistence, serialization, or any on-disk format;
- in-language (Loko-from-Loko) rewind calls;
- a stateful `interloko_lab` subcommand for rewind. The lab is
  stateless per invocation, so rewind has no useful CLI surface in
  this slice; tests in the compiler crate are the verification.
- `IncrementalCompilerSession::apply_mutation` cache reuse.

### Invariants

- `SessionArtifacts` privately stores the pristine source text once at
  first compile and preserves it byte-for-byte across every
  `apply_mutation` and `rewind_to` call. The pristine source is the
  rebuild base; nothing else.
- The internal `compose_source_from_log` helper deterministically
  produces a composed source string from the pristine base and an
  iterator of `&SessionMutation` requests. Both `apply_add_method` and
  `rewind_to` use it as the source-side step of the
  candidate-and-swap rebuild; the surrounding compile/validate/swap
  orchestration stays per-call-site because the two paths have
  different post-compile responsibilities (new-entry construction vs.
  dropped/retained partitioning). The helper is request-only by
  design: it does not see ids, so the later
  replay-against-changed-base slice can supply its own
  `AppliedMutation` set without inferring ids from log position.
- `rewind_to(Some(id))` requires `id` to name an entry currently in
  the live log; on success that entry and every earlier entry in log
  order are retained and the strict suffix is dropped. A target that
  does not name a live entry (because it was never assigned, or
  because it was dropped by a prior rewind) returns
  `RewindError::TargetOutOfRange`; rewind never silently re-interprets
  a missing id as "retain everything with a smaller numeric value."
  `rewind_to(None)` retains no entries and rebuilds from pristine.
- Mutation ids are stable session event ids. `SessionArtifacts` carries
  a private monotonic counter that advances on every successful
  `apply_mutation` and is *preserved* across `rewind_to`. Retained log
  entries keep their ids; dropped ids are permanently retired and
  never reused. The next `apply_mutation` after a rewind assigns the
  next never-used id, which is generally *not* equal to the live
  retained count (the log can have gaps).
- `MethodOrigin::Session` markers on retained methods are restored
  from the retained log entries (same log-driven retag the existing
  apply path uses).
- A failed rewind (target id outside the log, runtime validation
  failure on rebuild) leaves the live session untouched.
- The mutation log is never appended to by rewind. Rewind only
  truncates.
- `interloko_runtime` is not touched; rewind is a session-layer
  operation that uses the existing candidate-and-swap pipeline.

### Steps

1. Plan entry (this commit). No code, no docs surface.
2. Add a private `pristine_source: String` field to `SessionArtifacts`
   populated by the initial compile path. Refactor `apply_add_method`
   to compose candidate source from `pristine + log entries` instead
   of appending to `self.source.source`, and to preserve
   `pristine_source` across the candidate-and-swap. No new public API;
   all existing mutation-log tests pass unchanged.
3. Add `SessionArtifacts::rewind_to(target: Option<u32>) ->
   Result<RewindOutcome, RewindError>` plus the `RewindOutcome` and
   `RewindError` types. Source composition is shared with
   `apply_add_method` through the internal
   `compose_source_from_log(pristine, requests)` helper; the
   surrounding compile / validate / log-driven provenance retag /
   swap stays per-call-site. Tests:
   - applying two mutations and rewinding to the first id leaves the
     first method dispatching and the second gone;
   - rewinding to `None` returns the session to pristine: both methods
     gone, original dispatch restored, log empty;
   - rewinding to an out-of-range id returns `RewindError` and leaves
     the live session unchanged;
   - mutation log EOP records shrink in lockstep with the log
     truncation;
   - the next `apply_mutation` after a partial rewind assigns the
     next never-used id (one past the highest ever assigned), not a
     recycled dropped id; the post-rewind log has a deliberate gap
     where the dropped id used to be.
4. Update `docs/eop_mutation.md` with a "Rewind" section that makes
   the operations-on-log boundary explicit: rewind is not a
   `SessionMutation`, never appended to the log, and not exposed
   through the lab CLI in this slice. Update `PROGRESS.md` with an
   `Implemented: Rewind The In-Memory Mutation Log` section.

### Risks

- The pristine-source field is load-bearing. Any code path that
  re-derives session state must compose from pristine; any path that
  forgets to preserve pristine across a rebuild reintroduces the bug
  class this slice is meant to keep closed.
- The `compose_source_from_log` helper is the seed for replay-against-
  different-base. Its shape must not bake in assumptions specific to
  rewind: the source it composes against is "the session's pristine
  source today," but the helper itself accepts an arbitrary pristine
  string and a request iterator, so a replay slice can supply a
  different base. The helper is deliberately request-only (no ids in
  its signature) so replay cannot infer ids from log position.
- `RewindOutcome` / `RewindError` are new public types; later
  operations (retract-by-id, branch) must compose with the shape
  chosen here. Adding fields later is fine; renaming is expensive.
- The lab is stateless per invocation, so rewind has no useful CLI
  surface today. Docs should make this clear so the absence is not
  read as a missing feature.
- Rewind must not silently re-derive identifiers. Retained log
  entries' `mutation_id` values are stable across the rewind; the
  next mutation gets the next never-used id (from a preserved
  monotonic counter), *not* a recycled dropped id. Reusing ids would
  break the log's forward-transcript meaning and quietly conflate
  distinct historical events.

## Add Explicit In-Memory Mutation Log

### Goal

Promote the implicit per-session mutation history hidden inside the existing
`add-method` slice into an explicit, structured, in-memory log. The session
stores each successful mutation as an `AppliedMutation` record carrying the
request, the resulting `MutationOutcome`, and the assigned mutation id. The
log becomes the single source of truth for `MethodOrigin::Session`
provenance, replacing the `ast.methods.len() - mutation_sequence` arithmetic
that quietly underpinned the provenance retag fix. Each `AppliedMutation`
also projects as an EOP environment object so workbench tools can inspect
the mutation history through the same protocol surface they use for every
other compiler fact.

### Fence

Compiler sessions own the log shape, append order, log-driven provenance
retag, and log projection into EOP records. `interloko_lab` owns user-facing
reporting of the log size in the existing `add-method` outcome line. This
slice explicitly does not own:

- rewind / undo across mutations;
- branching: deriving a divergent session from a prefix of the log;
- replay against a changed base source file;
- log persistence, serialization, or any on-disk format;
- merge or conflict policy for divergent logs;
- in-language (Loko-from-Loko) mutation calls;
- a stateful `interloko_lab mutations <file>` subcommand or any other
  long-lived session surface;
- `IncrementalCompilerSession::apply_mutation` cache reuse (still
  `RecomputeStrategy::FullCandidateRebuild`).

### Invariants

- A successful `apply_mutation` appends exactly one `AppliedMutation` to the
  log; a failed `apply_mutation` appends none.
- The log is ordered by application; an entry's index equals its
  `mutation_id` and equals the `sequence` carried by the inserted method's
  `MethodOrigin::Session`.
- `SessionArtifacts::mutations()` returns the log in application order and
  is the public read surface.
- After a candidate-and-swap rebuild, every method named by any log entry's
  `outcome.inserted_method` carries `MethodOrigin::Session { sequence: id }`
  with the matching id. Provenance is computed from the log, not from
  method-count arithmetic.
- Each `AppliedMutation` projects to one EOP environment object with a
  stable id of the form `mutation:m<id>` and a kind/origin/storage facet
  set consistent with other workbench-derived environment objects.
- The log is in-memory only. No part of the slice writes log content to
  disk, embeds it in dump or report output as serialization, or uses it as
  a session resume input.
- `interloko_runtime` continues to honor its `no_std` constraint and gains
  no compiler dependencies.

### Steps

1. Plan entry (this commit). No code, no docs surface.
2. Add `pub struct AppliedMutation { mutation_id, request, outcome }`,
   `SessionArtifacts::mutations(&self) -> &[AppliedMutation]`, the
   `mutations: Vec<AppliedMutation>` field, log append on successful
   `apply_mutation`, and log-driven retag of every session-origin method
   after every candidate-and-swap rebuild. Remove the
   `saturating_sub`-based pristine-count computation now that the log is
   authoritative. Tests prove: two successful mutations produce two log
   entries with the expected ids and inserted methods; a failed mutation
   leaves the log untouched; provenance is restored from the log after a
   second mutation rebuild even when the recompile would otherwise reset
   `MethodOrigin` to `Source`.
3. Project each `AppliedMutation` as an EOP environment object with id
   `mutation:m<id>`, kind `mutation`, origin `session`, storage
   `transient-in-memory`, and an `inspection` facet that exposes the
   request shape and the recorded outcome. Workbench EOP dumps surface
   the log alongside existing environment objects. Tests cover EOP
   visibility before and after one or more mutations.
4. Extend the `interloko_lab invoke-affordance ... add-method` outcome
   line with `log_size=<N>` reporting the post-mutation log length.
   Update the existing CLI test to assert the new field. No new
   subcommand.
5. Update `docs/eop_mutation.md` with a "Mutation log" section describing
   the log shape, the in-memory fence, and the deferred follow-ups
   (rewind, branch, replay against changed base, persistence, in-language
   mutation, incremental cache reuse). Update `PROGRESS.md` with an
   `Implemented: Explicit In-Memory Mutation Log` section.

### Risks

- The log is the new ground truth for provenance. Any code that previously
  derived `MethodOrigin::Session` from method counts must move to the log
  in this slice; leaving even one arithmetic-based path resurrects the
  bug class the slice is meant to eliminate.
- The `AppliedMutation` shape is part of the public API. Later mutation
  affordances (`retract-method`, `edit-body`, `define-class`) and the
  eventual rewind/branch/replay slices must compose with this shape;
  adding fields later is fine, renaming them is expensive.
- EOP rendering of mutation records must not imply persistence. The
  `transient-in-memory` storage facet (or equivalent) is the contract; CLI
  and report wording must not call the log "saved" or "stored" outside
  the running session.
- The `interloko_lab` invocation is stateless: each invocation builds a
  fresh session and applies at most one mutation. The `log_size=1` value
  surfaced today is therefore always `1` for `add-method`. Documentation
  should make that ergonomic limit explicit so users understand the log
  is real even when the CLI cannot show multiple entries yet.

## Add Live EOP Method Mutation

### Goal

Make an Interloko compiler+runtime session a live mutable image for one narrow
case: adding a method to an existing generic through an EOP affordance. The
mutation updates semantic IR, runtime layout, and dispatch artifacts in place,
with incremental invalidation limited to the dispatch sites, runtime layout,
and downstream tape functions that actually depend on the mutated generic's
method table. This is the smallest slice that proves EOP can be a real mutation
protocol, not only an inspection surface.

### Fence

Compiler sessions own structured mutation requests, provenance, candidate
construction, rollback, diagnostics, and artifact-graph invalidation.
`interloko_runtime` owns executable method-table mutation, dispatch behavior,
and the layout invariants that mutation must preserve. `interloko_lab` owns
the user-facing executable affordance surface and result rendering only.
This slice explicitly does not own:

- persistent images, on-disk session storage, or session serialization;
- mutation affordances other than `add-method` (no `retract-method`,
  `edit-body`, `define-class`, or `redefine-class` in this slice);
- source-authored grammar changes or staged-region mutation;
- multi-session collaboration, locking, or capability policy;
- object migration, slot reshape, generic-signature change, or class-graph
  mutation;
- new tape opcodes; dispatch continues to lower through ordinary host calls;
- a new text-as-structure escape hatch. The affordance accepts a full Loko
  `define method ... end;` clause as user input, which is parsed immediately
  into structured AST through the ordinary parser. Method bodies do not bypass
  the AST/sem pipeline.

### Invariants

- A session that has not received a mutation produces bit-identical artifacts
  to a fresh session for the same source.
- After `add-method` succeeds, the inserted method participates in dispatch
  exactly like a source-origin method, distinguishable only via its
  `MethodOrigin::Session` provenance marker exposed in EOP records.
- `RuntimeLayout::add_method` rejects specializer vectors whose length does
  not match the target generic's parameter arity. Generic arity is a runtime
  layout fact carried by `RuntimeGeneric`, not a sem-only fact.
- The session-layer `add-method` mutation rejects insertions whose specializer
  vector exactly matches an existing method on the same generic. Direct
  runtime-layer ambiguity remains structurally possible for advanced callers
  but is not reachable through the affordance.
- `MutationOutcome::affected_artifacts` lists only artifacts that the
  dependency graph identifies as touched by the mutated generic's method
  table: the top-level pipeline artifacts that genuinely change (source,
  parse, AST, bind, lowered module, tape program, runtime result,
  diagnostics), the inserted method's per-method artifacts
  (`MethodLookup`, `MethodLowering`, `MethodTape`), and the affected
  generic's `GenericDispatcher`. Unrelated per-method and per-generic
  artifacts are not reported as affected.
- For artifacts not in `affected_artifacts`, post-mutation outputs are
  byte-equal to pre-mutation outputs. This slice does not yet skip their
  recomputation; the outcome reports a `RecomputeStrategy` that names the
  approach honestly (currently `FullCandidateRebuild`) and does not claim
  cache reuse.
- Mutation failures (parse, semantic, runtime validation, duplicate
  specializer) leave the session in its pre-mutation state with no partial
  updates visible. Rollback is achieved by computing a candidate session state
  and only swapping it in on success.
- All mutations remain in-memory; no on-disk persistence is introduced and no
  existing dump/report path is repurposed as implicit serialization.
- `interloko_runtime` honors its `no_std` constraint and gains no compiler
  dependencies.

### Steps

1. Plan entry. No code, no docs surface yet. *(landed as cea52f6.)*
2. `RuntimeLayout::add_method` with structured errors and baseline runtime
   tests. *(landed as 0da1d47.)*
3. Semantic IR `MethodOrigin::Session { sequence }` vocabulary plus label
   wiring; no session wiring yet. *(landed as 6e38ca5.)*
4. Add `arity: usize` to `RuntimeGeneric`, update all construction sites, and
   enforce arity in `RuntimeLayout::add_method`. Runtime tests cover an
   arity-mismatched insertion being rejected without mutating layout.
5. Add `SessionMutation::AddMethod { generic, method_source }` and
   `Session::apply_mutation(SessionMutation) -> Result<MutationOutcome,
   MutationError>`. `method_source` is a full `define method ...; end;`
   clause parsed immediately into structured AST. `MutationOutcome` carries
   the inserted method id, the mutation id, `affected_artifacts`, and the
   `RecomputeStrategy` actually used. Implementation is candidate-and-swap
   with full recompute. Tests prove successful insertion plus rollback for
   parse failure, semantic failure, runtime validation failure, and
   duplicate-specializer rejection, each checking that dispatch results and
   artifact ids are unchanged after the failed mutation.
6. Narrow mutation impact *reporting*, not cache reuse. Refine
   `MutationOutcome.affected_artifacts` to precisely list what the
   dependency graph says the mutation touches (top-level pipeline artifacts
   that genuinely change plus the new method's `MethodLookup`,
   `MethodLowering`, `MethodTape` and the affected generic's
   `GenericDispatcher`). Introduce `RecomputeStrategy` with a
   `FullCandidateRebuild` variant; the API does not claim cache reuse.
   Tests verify the affected list includes the new method's per-method
   artifacts and the affected generic's dispatcher, excludes unrelated
   methods' per-method artifacts, and asserts that unrelated lowered/tape
   outputs are byte-equal before vs after. True
   `IncrementalCompilerSession::apply_mutation` cache reuse is deferred to
   a follow-up plan/commit.
7. Expose `add-method` as an executable EOP affordance on `generic:gN`
   objects; wire through `interloko_lab invoke-affordance`; CLI tests cover
   success, parse failure, unknown-generic, arity mismatch, and duplicate
   specializer.
8. Update `PROGRESS.md`, add `docs/eop_mutation.md` describing the in-memory
   fence and the affordance contract, and confirm `docs/eop_workbench.md`
   reflects the now-executable affordance.

### Risks

- The fence between live mutable image and persistent image is load-bearing.
  Without explicit non-persistence enforcement, the slice will drift toward
  implicit serialization through reports, dumps, or session resume paths.
- Impact-reporting correctness is subtle. The dependency model in
  `affected_artifacts` must be honest in both directions: omitting an
  affected artifact silently lies about what the mutation touched, and
  including unrelated artifacts dilutes the contract and complicates the
  later incremental-reuse slice. Tests must cover both directions.
- The slice deliberately reports `RecomputeStrategy::FullCandidateRebuild`
  rather than claiming cache reuse. CLI and EOP rendering must not paper
  over this distinction with "reused" wording; that wording is reserved
  for the follow-up slice that actually wires
  `IncrementalCompilerSession::apply_mutation`.
- Affordance naming hardens early. Later mutation slices (`retract-method`,
  `edit-body`, `define-class`) must compose cleanly with `add-method`;
  affordance shape should be designed for the whole family, not just this
  case.
- The workbench `generated` flag is becoming load-bearing. This slice routes
  `MethodOrigin::Session` methods through it via a `SlotSetter`-specific
  match so user affordances stay available. Before more origin variants
  accumulate, the predicate should become a named accessor (for example
  `method_affordance_policy`) so each new origin makes an explicit policy
  choice rather than inheriting one through pattern shape.
- Mutation-rollback semantics must be tested explicitly. A failed mutation
  that leaves any visible state behind silently undermines every later
  mutation affordance.

## Eliminate Staged Expression Text Fallback

### Goal

Remove the transitional staged-expression text fallback. Trusted staged
adapters should emit structural frontend expression nodes for the subset they
need today, AST lowering should consume those nodes directly, and materialized
Loko should remain only as an inspectable debug/report projection.

### Fence

`grammar_frontend` owns structural frontend forms, structural staged expression
records, adapter parsing, and materialized debug source. `ast` owns lowering
those forms into AST definitions and expressions. This slice does not add new
Loko semantics, source-authored grammars, macro hygiene, semantic actions, or
new lowering/tape behavior.

### Invariants

- Staged regions are not reparsed as generated Loko source.
- Calculator staged bodies are structural frontend expressions.
- Records staged forms remain structural class forms.
- Materialized Loko is still available for reports and debugging.
- `sem`, `lower`, and tape emission continue to consume ordinary AST and
  semantic products.

### Steps

1. Replace the legacy staged-expression text variant with structural integer,
   name, call, and binary expression nodes.
2. Lower structural staged expressions directly into AST expressions.
3. Update staged adapter source-data/report projection and regression tests.
4. Update grammar MOP docs and progress notes to remove the text-fallback
   caveat.
5. Run fmt, clippy, workspace tests, docs, corpus checks, and commit in clean
   slices.

### Risks

- The structural expression subset is intentionally small; docs must not imply
  a complete source-authored macro system.
- Debug materialized source should stay faithful enough for reports without
  becoming the compiler input again.

## Add Runtime Object Protocol Helpers

### Goal

Make `interloko_runtime` easier to use as a small executable object protocol,
not just a bucket of layout structs and tape host-call branches. Add structured
runtime operation errors, public inspection helpers, slot capability reporting,
and a clearer host-backed adapter contract.

### Fence

`interloko_runtime` owns executable runtime object protocol facts and failures;
it explicitly does not own semantic IR, workbench rendering, diagnostics
catalogs, source syntax, or persistence/image policy.

### Invariants

- Public runtime operations report structured runtime errors before mapping to
  `execution_tape::host::HostError`.
- Host-backed objects and native objects share the same slot-read protocol.
- Writable slot capability is explicit and distinguishes constant,
  host-backed, and non-native storage.
- Runtime inspection helpers expose classes, roots, superclass chains, slots,
  generics, and methods without forcing callers to index raw vectors directly.
- `interloko_runtime` remains `no_std` and does not depend on compiler crates.

### Steps

1. Add `RuntimeError`, slot capability types, and layout/object inspection
   helpers.
2. Change `InterlokoHost` public object APIs to return `RuntimeError` and map
   those errors to `HostError` only at the tape host boundary.
3. Update host object adapters, examples, and compiler tests for the structured
   error contract.
4. Use runtime helpers in compiler/session projection where it reduces raw
   layout indexing.
5. Update docs/progress and run fmt, clippy, workspace tests, and docs.

### Risks

- Keep this as a protocol cleanup, not a full persistent runtime or capability
  system.
- Avoid threading localized diagnostics into the runtime crate; runtime errors
  should stay structural.

## Move Runtime Ownership Forward

### Goal

Make `interloko_runtime` own more executable object-system behavior instead of
leaving it implicit in compiler lowering, session inspection, or host-call
branches. Add layout validation, runtime slot APIs, runtime generic dispatch
lookup/explanation data, and direct runtime conformance tests.

### Fence

`interloko_runtime` owns executable class layout invariants, runtime object
storage, slot read/write behavior, class ancestry, and method-table dispatch
facts. It explicitly does not own Loko syntax, AST, semantic IR, lowering,
compiler sessions, EOP projection, HTML reports, diagnostics catalogs, or
source-authored grammar behavior.

### Invariants

- `class:c0` is the `<object>` root and has no superclass.
- Every non-root class has a valid superclass chain that reaches `<object>`.
- Runtime slots point at valid source classes and source slots.
- Runtime methods point at valid generics and valid class specializers.
- Slot reads/writes go through runtime APIs used by tape host calls.
- Generic dispatch lookup has one structured implementation reusable by
  compiler/session explanations.
- The compiler bridge remains the only `sem::Module -> RuntimeLayout` adapter.
- `interloko_runtime` stays `no_std` and does not gain compiler dependencies.

### Steps

1. Add `RuntimeLayout::validate` and direct runtime tests for root, class,
   slot, generic, and method invariants.
2. Add runtime slot read/write APIs and update host-call branches to delegate to
   them.
3. Add runtime dispatch lookup/explanation records with applicable/rejected
   candidates, distances, selected method, and ambiguity/no-method outcomes.
4. Update compiler/session dispatch inspection to use runtime lookup facts where
   practical while keeping rendering and artifact wiring in the compiler.
5. Update docs/progress notes and run fmt, clippy, workspace tests, docs, and
   corpus.

### Risks

- Dispatch explanation must stay structurally compatible with current
  workbench output while changing the owner of the lookup facts.
- Runtime validation should catch bad layouts without making valid compiler
  output pay excessive cost during normal execution.
- Keep the first slice narrow: no new public macro semantics, persistent
  runtime image, host adapter split, or scheduler.

## Add Timing Instrumentation And Execution Tracing

### Goal

Expose how much work each compiler artifact did and what the emitted tape did
at runtime. Timing should make the static HTML report and lab CLI more honest:
artifact keys show identity, work units show scale, durations show observed
cost, and execution traces show the runtime path through ordinary tape calls
and host calls.

### Fence

The session/workbench layer owns artifact timing, work summaries, and captured
runtime trace records. It explicitly does not own scheduling policy, persistent
profiling storage, external tracing backends, or `execution_tape` VM internals
beyond using its public `TraceSink` API.

### Invariants

- Artifact keys and fingerprints do not include timing data.
- The compiler crate remains `no_std` by default; real wall-clock timing is
  enabled through an explicit `std` feature for tools.
- Non-`std` builds still expose stable timing fields with zero/unknown values.
- Trace capture uses ordinary `execution_tape::trace::TraceMask` values.
- Reports and CLI output expose compact summaries first, with raw trace/event
  detail available on demand.

### Steps

1. Add per-artifact timing metrics beside existing work-unit/cache reports.
2. Thread timing through artifact graph dumps, diff/cache reports, and session
   artifacts without changing artifact fingerprints.
3. Add an Interloko trace sink that records run, call-frame, host-call, and
   optional instruction events from `execution_tape`.
4. Add traced runtime execution paths and session trace dumps.
5. Add `interloko_lab run --trace`, `interloko_lab trace <file>`, and report
   timing/trace presentation.
6. Add tests for timing presence, trace event capture, default non-traced
   behavior, and report/CLI output.
7. Update docs/progress and commit as timing, tracing, and lab/report slices.

### Risks

- Wall-clock timing can make tests flaky; tests should assert presence and
  shape, not exact durations.
- Trace output can become noisy; default reports should prefer summarized
  timelines.
- The timing feature must not pull `std` into foundational builds by default.

## Build The Interloko HTML Report Demo

### Goal

Make `interloko_lab report <file> --output <path>` the clearest way to show a
Loko program moving through the compiler/workbench stack. The command writes one
self-contained HTML file with embedded CSS, JavaScript, and structured data.
The staged-records fixture is the flagship demo, but ordinary Loko files should
also get useful empty states.

### Fence

`tools/interloko_lab` owns this presentation surface. It consumes existing
session artifacts, EOP objects, stage dumps, grammar parse reports, and runtime
results. It does not add compiler semantics, new grammar behavior, a web
server, live editing, external frontend dependencies, or shared presentation
APIs unless extraction becomes clearly justified later.

### Steps

1. Add report CLI parsing and a lab-local report renderer.
2. Render source, staged regions, frontend forms, materialized source,
   generated metaobjects, diagnostics, artifact keys, lowered IR, and tape
   disassembly into a polished self-contained report.
3. Embed deterministic JSON data and a small relation-highlighting script.
4. Add lab fixture tests for the staged-records report.
5. Document the report command as the preferred demo path and update progress.
6. Run fmt, clippy, workspace tests, docs, corpus/examples, generate the sample
   report, and commit.

### Risks

- The report can look more capable than the compiler if it hides which stages
  are still materialized-source compatibility paths.
- Source span highlighting should stay honest: staged/source/form relations are
  shown where current artifacts expose enough provenance.
- Keep presentation code out of core crates until a real live tool needs a
  shared report/data API.

## Build Structural Frontend Forms And Records Grammar

### Goal

Move staged grammar adapters from text emission toward an inspectable expander
substrate. Adapters emit structural frontend forms with source provenance; the
frontend materializes those forms back to ordinary Loko source only at the
existing parser boundary. Add a trusted `records` staged grammar that emits
class definitions with typed slots through those forms.

### Fence

`grammar_frontend` owns trusted staged adapter registration, frontend form
records, staged-region routing, form-to-source materialization, grammar-stack
records, structural emissions, and parse provenance. It explicitly does not own
AST lowering, semantic meaning, source-authored grammars, macro hygiene,
arbitrary host execution, plugin loading, or parser generation.

### Invariants

- `syntax -> AST -> sem -> lower -> tape` stays unchanged for this slice.
- Staged adapters return structured frontend forms as the primary result.
- Materialized source is a compatibility artifact and remains inspectable.
- Original source spans stay attached to forms and staged-region records.
- Calculator behavior remains working while moving off raw text emission.
- `records` is a trusted Rust-hosted adapter, not source-authored grammar
  syntax.
- Diagnostics carry stable codes and structured args, not English-text parsing.

### Steps

1. Add frontend form structs/enums for emitted functions, classes, slots,
   expression text, spans, and source-data projection.
2. Change staged emissions to carry forms plus materialized source; update trace
   and EOP output to show forms before emitted text.
3. Replace one-off adapter lookup with a small built-in registry that can hold
   `calculator` and `records`.
4. Add `RecordsGrammar` with direct/effective rules and simple syntax:
   `record name`, typed `field: <type>` lines, `end`.
5. Add records lab/corpus fixtures that compile/run and expose classes/slots in
   MOP/EOP.
6. Update docs/progress, run fmt, clippy, workspace tests, docs, corpus, lab,
   and example binaries.

### Risks

- Frontend forms could look more powerful than they are; docs must state that
  AST still comes from materialized Loko text.
- Records syntax should stay small enough to prove the path without becoming a
  second language.
- Span mapping for generated forms remains approximate at the parser boundary;
  exact generated-to-original diagnostic remapping is deferred.

## Generalize Staged Grammar Activation

### Goal

Put the staged calculator prototype behind an internal grammar registry and
make activation explicit frontend metadata. A `// grammar-activate: calculator`
directive selects the registered adapter for one or more later staged regions.
Each region is parsed by that adapter and emits ordinary Loko source that
continues through the existing syntax -> AST -> sem -> lower -> tape pipeline.

### Fence

`grammar_frontend` owns the staged adapter protocol, built-in registry,
activation directives, staged-region routing, grammar-stack records, structural
emissions, and final parser invocation. It does not own Loko semantics, AST
lowering, semantic actions, source-authored grammar declarations, host code
execution, capability policy, or a general parser generator.

### Prototype Syntax

The activation directive and staged markers are workbench/frontend metadata,
not Loko syntax:

```text
// grammar-activate: calculator

// staged-loko: calculator
helper = 40
// end-staged-loko

// staged-loko: calculator
main = helper() + 2
// end-staged-loko
```

The two calculator regions emit:

```text
define function helper()
  40
end

define function main()
  helper() + 2
end
```

### Steps

1. Add an internal `StagedGrammar` adapter protocol and registry with the
   calculator adapter as the only built-in entry.
2. Replace prefix symbol scanning with `// grammar-activate: calculator`.
3. Route one or more `// staged-loko: calculator` regions through the selected
   adapter and materialize ordinary Loko source for the existing parser.
4. Expose adapter name, activation directive span, source span, emitted source,
   and structural source-data in parse trace and EOP inspection.
5. Update lab/corpus fixtures, tests, and docs.
6. Run fmt, clippy, workspace tests, docs, corpus, examples, and lab tests.

### Risks

- Source spans for emitted ordinary Loko forms remain materialized-source spans
  in this prototype; exact original-source span mapping is deferred.
- Activation directives must stay visibly session/frontend metadata so this
  does not imply source-authored macros or arbitrary host semantic actions.

## Build Grammar Follow-Up Slices

### Goal

Make the grammar MOP slice more load-bearing in four clean commits: project a
trusted derived grammar, let sessions select a grammar stack, add
grammar-aware diagnostic/trace facts, and add a tiny source-data layer that can
carry structural parser/protocol facts before source-authored syntax extension.

### Fence

`grammar_frontend` owns grammar projections, effective grammar rules, selected
grammar stacks, parse activations, parse provenance, and source-data records
needed by the frontend. Session and workbench code may select and inspect those
facts, but AST, semantic IR, lowering, and tape emission continue to consume
the existing syntax products without knowing grammar lookup internals.

### Steps

1. Add `DerivedHostGrammar` as a read-only host-provided grammar projection
   extending `BaseLokoGrammar`, with direct and effective rule records kept
   distinct.
2. Add session/workbench input for selecting the base or derived grammar stack,
   and make cache reporting treat grammar-stack changes as parse dependencies.
3. Add grammar-aware diagnostic facts and parse-trace output without rendering
   compiler diagnostics from prose.
4. Add a small source-data representation for structural frontend facts; do
   not implement macros, semantic actions, or source-authored grammars yet.
5. Run fmt, clippy, workspace tests, docs, corpus, examples, and lab tests.

### Risks

- The derived grammar is only a projection/delegation fixture; output must not
  imply it rewrites the parser.
- Grammar-stack selection must be explicit session input so ordinary source
  edits do not look like grammar edits.
- Source-data should stay structural and inert until staged parsing has a real
  capability boundary.

## Add Read-Only Grammar MOP Projection

### Goal

Expose the current hand-written Loko parser as read-only grammar metaobjects
without changing parser behavior. The first slice should make the base language
environment, base grammar, projected grammar rules, resolved grammar stack,
activations, and parse provenance inspectable through the existing session,
MOP/EOP, artifact, and workbench surfaces.

### Fence

`interloko_compiler::grammar_frontend` owns language-environment identity,
grammar metaobjects, grammar-stack resolution, parser invocation, parse
activations, and parse provenance. It explicitly does not own AST lowering,
semantic lowering, source-authored grammars, macros, semantic actions, host code
execution, staged parsing, OMeta/Ohm compatibility, or a parser rewrite.

### Steps

1. Add grammar identity and report records in a new `grammar_frontend` module.
2. Wrap the existing `syntax::parse_file` path and return a
   `GrammarParseReport` plus the existing `SyntaxTree`.
3. Add explicit query artifacts for environment resolution, grammar stack, and
   grammar parse, then thread them through session artifact reports.
4. Project `BaseLokoEnvironment`, `BaseLokoGrammar`, grammar rules, and
   activations through MOP/EOP records.
5. Add `interloko_lab` affordance handling for grammar stack, grammar rule, and
   parse trace inspection.
6. Cover stable grammar object IDs across harmless edits and normal
   source-to-tape compilation through tests.
7. Document the grammar MOP boundary, deferred staged parsing, and validation
   results before committing.

### Risks

- Exposing too much parser internals would freeze private implementation
  details before the grammar protocol earns them.
- A derived grammar fixture without real parser delegation would be misleading,
  so this slice only projects the base grammar and reserves overrides for a
  later goal.
- Grammar artifacts must stay separate from ordinary method-body edit reports,
  or incremental output will become noisy and less useful.

## Extract Interloko Runtime Host Support

### Goal

Move executable runtime host support out of `interloko_compiler` and into
`crates/interloko_runtime`, while leaving semantic IR and sem-to-layout
construction in the compiler.

### Fence

`interloko_runtime` owns host-call symbols/signatures, runtime layout data,
runtime-representable value kinds, `InterlokoHost`, object storage, boxed top
values, string/symbol/vector runtime objects, metaobject host calls, and
host-backed object adapters. `interloko_compiler` owns syntax, semantic IR,
semantic `ValueKind`, lowering, sessions, diagnostics, and the adapter that
turns `sem::Module` into runtime layout.

### Steps

1. Create `crates/interloko_runtime` and add workspace/compiler dependencies.
2. Move runtime host symbols, signatures, layout structs, object heap/storage,
   boxed values, literals, MOP query host calls, and `InterlokoHost`.
3. Add runtime `ValueKind` and convert semantic slot kinds during layout
   construction.
4. Replace `RuntimeLayout::from_sem` with
   `interloko_compiler::runtime::runtime_layout_from_sem`.
5. Keep `interloko_compiler::runtime` as only the sem-layout adapter; import
   executable runtime APIs from `interloko_runtime` directly.
6. Point tape emission at `interloko_runtime` symbols/signatures.
7. Update docs/progress, run gates/examples, audit, and commit.

### Risks

- Moving semantic `ValueKind` directly would leak compiler-only states into the
  runtime crate.
- Moving layout construction into runtime would make runtime depend on semantic
  IR and weaken the new dependency direction.
- Compatibility re-exports must not hide which crate owns host execution.

## Establish Interloko Core Substrate Boundary

### Goal

Introduce Interloko as the name and crate boundary for the boring shared
substrate under the current compiler/runtime prototype. This is the first
dependency-direction step toward separating model facts from compiler and
runtime execution. Loko is the source notation and `.loko` file format.

### Naming

- Interloko is the core technical substrate for programmable object spaces.
- Vivarium is the later collaborative environment and product layer.
- Loko is the current source notation and language surface.

### Fence

`interloko_model` owns durable shared model identifiers and small value-class
facts needed by both compiler and runtime-shaped code. It explicitly does not
own syntax, semantic IR, lowering, tape emission, runtime host execution,
session orchestration, collaboration, identity, permissions, or persistence.

### Steps

1. Add a naming/architecture note that records the Interloko/Vivarium/Loko
   boundary and provisional crate naming.
2. Create `crates/interloko_model` as a `no_std` substrate crate.
3. Move durable shared ids (`ClassId`, `SlotId`, `GenericFunctionId`,
   `MethodId`) and `BuiltinValueClass` into `interloko_model`.
4. Re-export the moved types from `interloko_compiler::sem` so existing call sites
   remain calm during the extraction.
5. Keep `ValueKind`, runtime layout records, semantic IR, lowering, tape
   emission, sessions, diagnostics, and runtime host execution in
   `interloko_compiler`.
6. Update progress notes and run all workspace gates, corpus, and examples.

### Risks

- Moving `ValueKind` now would pull compiler-only callable/error states into
  the model crate. Leave it until there is a cleaner runtime/model value kind.
- Moving `RuntimeLayout` now would still drag semantic construction policy and
  runtime host details across the new boundary. Leave it for the later runtime
  extraction.
- Renaming packages before the boundary proves itself would create churn
  without architectural value.

## Name The Read-Only MOP Protocol Surface

### Goal

Turn the first-class metaobject slice into an intentional read-only MOP
protocol. Loko code should ask object-system questions through stable protocol
names such as `object-class`, `class-direct-slots`,
`class-effective-slots`, `slot-name`, `generic-function-methods`, and
`method-generic-function`. The old compiler-owned metadata remains the source
of truth; this slice only exposes it more coherently.

### Non-Goals

- No runtime class, generic, method, or slot mutation.
- No user-defined metaclasses.
- No method combination or `next-method`.
- No dynamic add/remove method protocol.
- No generalized reflective invocation.
- No persistent image mutation or capability policy.
- No optimizer assumptions over reflective protocol calls.

### Fence

`sem` owns stable protocol meaning, static checking, and metaobject identity. It
explicitly does not own runtime heap representation or live image mutation.
Runtime owns concrete read-only query execution. Session, MOP, and EOP dumps
are views over the same semantic/runtime facts, not a parallel protocol model.

### Steps

1. Add canonical protocol names for object, class, slot, generic-function, and
   method inspection without compatibility aliases for the previous intrinsic
   names.
2. Split class slot queries into direct source slots and effective finalized
   slots.
3. Add slot descriptor queries for name, declared type text, init keyword,
   mutability, getter name, and setter generic when one exists.
4. Add `object-class` as the protocol spelling for asking a runtime value for
   its class metaobject.
5. Lower all protocol operations through ordinary runtime host calls and tape
   host signatures, with no MOP-specific tape opcodes.
6. Update corpus examples, session dumps, lowered dumps, progress notes, and
   user-facing docs so Loko code and workbench inspection tell the same story.
7. Run fmt, clippy, tests, docs, corpus, examples, then commit coherently.

### Risks

- `slot-type` cannot yet return a first-class type object for `<object>`, so it
  should expose the declared type text until Interloko has a real type metaobject
  layer.
- Getter accessors are not generic functions yet. `slot-getter` should expose
  the getter name honestly rather than pretending a getter generic exists.
- Direct/effective slot descriptors must still name effective runtime storage
  positions so existing slot reads and layout remain simple.

## Expose First-Class MOP Metaobjects To Loko Code

### Goal

Make class, generic-function, and method metaobjects directly available to
ordinary Loko code in a narrow read-only slice. Loko code should be able to
ask basic questions such as `class-name(<circle>)`, `generic-methods(area)`,
and `method-specializers(...)` without going through workbench-only dumps.

### Non-Goals

- No runtime class/generic/method mutation.
- No user-defined metaclasses.
- No AMOP-compatible public protocol.
- No method combination changes.
- No persistent image/object identity.
- No special MOP or EOP tape opcodes.
- No automatic Rust reflection.

### Fence

At the Interloko semantic level, classes, generics, and methods are values. Their
stable ids remain internal compiler/runtime representation details. Lowering may
still turn known metaobjects into raw ids for existing hot paths such as `make`,
`class-of`, `instance?`, casts, slot access, and dispatch.

Runtime host calls own the physical representation of read-only metaobjects.
Semantic IR owns which metaobject is referenced. Session/MOP/EOP dumps are views
over the same facts and must not describe a different object system.

### Steps

1. Add semantic value categories and expression forms for class, generic,
   method, and slot-descriptor metaobjects without breaking existing optimized
   class-id paths.
2. Add runtime metadata for generics, methods, and slot descriptors alongside
   existing runtime class layout.
3. Add ordinary intrinsic calls for read-only introspection: `class-name`,
   `class-superclass`, `class-slots`, `generic-name`, `generic-methods`,
   `method-generic`, and `method-specializers`.
4. Lower those operations through normal runtime host calls and update tape
   emission/disassembly without adding new tape opcodes.
5. Keep generated setter generics/methods visible as metaobjects.
6. Update semantic, lowered, MOP, EOP, and session dumps so in-language
   metaobjects and workbench inspection agree.
7. Add corpus/tests for class name/superclass/slots, generic methods,
   multi-argument method specializers, generated setters, and host-backed
   classes.
8. Update progress notes with the implemented MOP surface and deferred AMOP,
   mutation, metaclass, and persistence design.
9. Run `cargo fmt --all`, `cargo clippy --workspace --all-targets
   --all-features -- -D warnings`, `cargo test --workspace --all-features`, and
   `cargo doc --workspace --all-features --no-deps`, then commit coherently.

### Risks

- A second visible notion of "class object" would muddy the MOP. Avoid that:
  public Interloko semantics should talk about metaobjects; ids are internal.
- Returning vectors of method/slot metaobjects may require minimal vector
  inspection helpers. Keep any helpers generic and small.
- Host-backed classes must participate in the same metadata path without
  implying automatic Rust object reflection.

## Add True Multi-Argument Generic Dispatch

### Goal

Make generic dispatch consider every required argument, not just the first
dispatch parameter. Methods carry specializer vectors, dispatchers check all
specialized arguments, and diagnostics reject ambiguous method sets that have no
unique most-specific method for a provable runtime class tuple.

### Fence

`sem` owns method specializer vectors, applicability, specificity, and ambiguity
diagnostics. `lower` owns the executable dispatcher shape. Runtime and tape only
execute ordinary lowered calls, branches, casts, class checks, and traps. This
slice does not own method combination, `next-method`, singleton/union dispatch,
keyword/rest dispatch, dispatch caches, or sealed-domain optimization.

### Invariants

- Single-argument dispatch remains source- and dump-compatible where possible.
- A method applies when every specialized parameter accepts the actual class.
- A method is more specific only when it is no less specific in every position
  and stricter in at least one position.
- An ambiguous method set is diagnosed before lowering emits a dispatcher.
- Setter generics keep the `(new-value, object)` call shape and still select the
  object-specific generated setter method.
- Tape emission uses ordinary execution-tape functions and host calls; no
  generic-specific tape opcode is introduced.

### Steps

1. Relax AST method lowering to retain all specialized parameters.
2. Extend semantic methods with specializer vectors and generic/method arity
   checks.
3. Add semantic applicability, specificity, and finite-lattice ambiguity checks.
4. Lower dispatchers by testing each specialized argument in specificity order.
5. Update session/MOP/EOP/dispatch dumps to show specializer vectors.
6. Add corpus/tests for exact pairs, subclass specificity, fallback methods,
   ambiguity, arity rejection, and setter preservation.
7. Update progress notes, run final gates, audit, and commit.

### Risks

- `<object>` is currently represented as top value kind rather than a nominal
  class id, so top method specializers need an explicit representation.
- Partial specificity order must not silently choose one of two incomparable
  applicable methods.
- Existing workbench output has many single-specializer assumptions that need to
  become vector labels without obscuring the common single-dispatch case.

## Add Dylan-Style Writable Slot Assignment

### Goal

Add `obj.slot := value` as Dylan-style setter sugar backed by compiler-generated
slot setter generics and methods. Mutable native slots become writable through
the same generic-call surface users can inspect and call explicitly.

### Fence

Syntax owns recognition of the `:=` surface form. Semantic lowering owns setter
protocol facts and diagnostics. Lowered IR, tape emission, and runtime host
support own mutation of native object storage. This slice explicitly does not
own generalized setter places, user-defined setter methods, writable
host-backed slots, transactions, persistence, or optimizer removal of setter
generic overhead.

### Invariants

- Getter accessors keep their current behavior.
- A mutable native slot synthesizes a visible setter generic named `<slot>-setter` with
  `(new-value, object)` argument order and a setter method returning
  `new-value`.
- Constant slots and host-backed slots remain read-only.
- Assignment and explicit setter-call spelling share the same semantic path.
- Tape uses ordinary host calls for slot writes; no special tape opcode is
  introduced.

### Steps

1. Extend syntax/AST for `obj.slot := value` while leaving generalized setter
   syntax deferred.
2. Synthesize setter metaobjects during semantic collection/finalization and
   route assignment/calls through them.
3. Add semantic diagnostics for read-only, unknown, non-object, host-backed, and
   value-type mismatch writes.
4. Lower native slot writes, emit a runtime `slot_set` host call, and mutate
   native object storage.
5. Expose setter protocol facts in dumps/EOP enough to inspect them.
6. Add corpus and unit/CLI coverage, update progress notes, then run final
   gates and commit.

### Risks

- Existing generic dispatch is first-argument dispatch only; setter lowering
  must preserve the user-visible generic shape without pretending full
  multi-argument dispatch exists.
- Mutable runtime storage must reject boxed/top values and typed values with the
  same value-kind checks used for construction.
- MOP visibility should be honest about compiler-synthesized setters and
  deferred user-defined setter methods.

## Add Richer Value Types

### Goal

Add Interloko's first non-numeric language values: strings, symbols, keywords, and
simple immutable vectors. These values must flow through syntax, AST, semantic
IR, lowered IR, tape/runtime execution, dumps, examples, corpus, and
diagnostics.

### Fence

`interloko_compiler` owns source syntax, semantic value kinds, lowered value
operations, runtime representation, and tape emission for these values. It
explicitly does not own general collection libraries, mutable sequence
operations, persistent cross-session interning, package-qualified symbols,
string interpolation, locale semantics, or optimizer work.

### Invariants

- String escape decoding is explicit and diagnosed for malformed literals.
- Symbols and keyword values have stable per-session identity and are distinct
  from function/generic keyword arguments.
- Vectors are immutable ordered values in this slice.
- Slot storage and function/generic contracts use the same `ValueKind`
  machinery as existing integers, booleans, objects, and classes.
- Tape lowering uses ordinary constants/runtime calls/host calls, not new
  special-purpose generic opcodes.

### Steps

1. Extend lexer/parser/AST for strings, symbols, keywords, and vector literals
   with focused diagnostics.
2. Extend semantic value kinds, type annotations, slot/function/generic
   checking, equality rules, and dumps.
3. Extend lowered IR and tape/runtime representation for executable values.
4. Add corpus, examples, session/REPL/workbench coverage, and progress notes.
5. Keep commits coherent: syntax/AST, sem/diagnostics, lower/runtime, docs/tests.

### Risks

- Vectors can easily become a general sequence library; this pass only needs
  literals, storage, passing, returning, and display/dumps.
- Equality should be conservative: only define cases with clear identity/value
  semantics.
- Keep interning deterministic inside a compiled module/session, not globally
  persistent.

## Add Initial Interloko REPL Tool

### Goal

Add a plain-stdio `interloko_repl` tool that drives the existing incremental
session API, supports durable definitions plus generated scratch expressions,
and exposes the same stage/artifact/MOP/EOP inspection surfaces as the
workbench.

### Fence

`interloko_repl` owns terminal interaction, input buffering, scratch-source
generation, and command routing. It explicitly does not own parsing, binding,
runtime dispatch, diagnostics rendering policy, MOP/EOP construction, artifact
graph logic, persistent image state, or language semantics.

### Invariants

- Definitions append to one durable in-memory source buffer with a stable
  `SourceFileId`.
- Expression input compiles durable source plus one generated scratch function;
  scratch definitions are replaced each time.
- All compilation still flows through source -> syntax -> AST -> semantic IR ->
  lowered IR -> tape.
- Named scratch execution uses a small compiler/session runtime API, not a
  fake `main`.
- REPL diagnostics render through `interloko_diagnostics`.
- The first slice uses plain stdin/stdout, with no line-editing dependency.

### Steps

1. Add a compiler/session API for running a named zero-argument function from
   existing `SessionArtifacts`.
2. Add `tools/interloko_repl` to the workspace with a small state machine for
   durable source, pending multiline definitions, scratch expressions, and
   colon commands.
3. Reuse `SessionArtifacts` dumps, dispatch inspection, artifact reports, and
   environment objects for commands.
4. Add transcript-style CLI coverage for definitions, expression evaluation,
   source/dump/artifact commands, and quit/clear behavior.
5. Document the workflow and current limits, then run fmt, clippy, and tests.

### Risks

- Do not accidentally treat expression evaluation as top-level mutation.
- Do not duplicate `interloko_lab` rendering logic beyond small text formatting
  helpers.
- Keep result printing honest: integer results print directly; non-integer or
  missing-function cases report runtime errors.

## Make Diagnostics Catalog-First

### Goal

Finish the diagnostic localization boundary by making compiler diagnostics
language-neutral facts only. User-facing wording moves to MessageFormat
catalogs loaded by presentation tools.

### Fence

`interloko_compiler` owns diagnostic codes, arguments, spans, labels, notes, and
artifact identity. It explicitly does not own MessageFormat compilation, TOML
resource loading, selected language/catalog state, or fallback English text.
`interloko_diagnostics` owns catalog-backed rendering for tools such as
`interloko_lab` and `interloko_corpus`.

### Invariants

- `Diagnostic` has no embedded rendered message.
- Producer sites emit a stable `DiagnosticCode` plus structured args.
- Rendered wording is never part of artifact fingerprints.
- Missing catalog entries, missing args, and formatting failures are hard
  presentation errors.
- English and pseudo catalogs both cover every stable diagnostic code.
- EOP diagnostic records expose facts, not pre-rendered text.

### Steps

1. Remove `Diagnostic::message` and migrate producer callsites to code/arg
   constructors.
2. Update session fingerprints, work counters, EOP diagnostic projection, and
   corpus checks so they use diagnostic facts and catalog rendering where text
   is intentionally tested.
3. Make `interloko_diagnostics` strict, add pseudo catalog coverage, and expose
   explicit catalog/locale selection for `interloko_lab`.
4. Add tests for code/arg-only rendering, loud missing catalog/arg failures,
   pseudo rendering, and language-independent artifact identity.
5. Update docs, then run fmt, clippy, workspace tests, and docs.

### Risks

- Some existing tests assert English strings directly; migrate them to codes
  and args unless the test is specifically about presentation.
- Syntax diagnostics may still carry syntactic fragments as args, but not full
  fallback sentences.
- Keep `message-format` out of compiler crates.

## Build Structured Diagnostics And MF2 Localization Boundary

### Goal

Replace string-only compiler diagnostics with stable diagnostic facts that can
still render the current English messages. Prepare a clean boundary for a later
MessageFormat 2 renderer without adding `message-format` as a core compiler
dependency.

### Fence

`interloko_compiler::diagnostic` owns diagnostic identity, spans, labels, arguments,
and the default English rendering contract. It explicitly does not own localized
catalog loading, terminal snippet styling, LSP adaptation, fix-it edits, or
MessageFormat 2 runtime integration.

### Invariants

- Diagnostic codes are stable enough for tests, corpus expectations, EOP
  records, and tools.
- This earlier migration kept `Diagnostic::message` temporarily; the later
  catalog-first plan above removes it.
- Structured arguments use named values rather than parsing prose.
- Labels describe source spans; the primary label remains the diagnostic span.
- EOP diagnostic objects expose both facts and rendered text.
- `interloko_compiler` must remain free of a production dependency on
  `../message-format`.

### Steps

1. Add `DiagnosticCode`, diagnostic args, labels, notes, and render helpers in
   `interloko_compiler::diagnostic`.
2. Provide constructors/builders so new diagnostics can be structured while old
   string callers still compile.
3. Migrate syntax, AST, semantic, lowering, tape, runtime/session diagnostic
   creation to stable codes and structured args.
4. Teach diagnostic fingerprints, work counters, EOP projection, dumps, and
   `interloko_lab` output to include codes and structured facts.
5. Extend `interloko_corpus` to support `// expect: diagnostic code ...` while
   keeping `diagnostic contains ...`.
6. Add focused unit/CLI/corpus coverage for code expectations, args, labels,
   rendered compatibility text, and EOP diagnostic records.
7. Document diagnostic facts vs presentation and the future MF2 renderer/catalog
   boundary, then run all final gates and examples.

### Risks

- Migrating every diagnostic in one pass can become noisy; keep helper
  constructors small and mechanical.
- Codes must be named semantically, not after current English wording.
- The MF2 boundary should stay documented, not implemented, in this slice.

## Build Initialization-Plan Substrate For Dylan-Style Initialize

### Goal

Make object construction use explicit class initialization plans and init-argument
records. Preserve today's slot-initialization behavior, but represent it as a
computed MOP fact that future Dylan-style `make`/`initialize` can consume.

### Fence

Semantic lowering owns computed initialization plans as MOP facts derived from
finalized effective slots. Lowering and runtime construction consume those
plans. This slice explicitly does not own user-defined `initialize`, overridable
`make`, custom allocation, init-arg forwarding, `next-method`, method
combination, reinitialization, class redefinition, slot setters, multiple
inheritance, sealed optimization, or new execution_tape opcodes.

### Invariants

- Direct slots are source facts.
- Effective slots are finalized class facts.
- Initialization plans are computed class/MOP facts.
- Init-argument records are semantic call-site facts for `make`.
- Runtime construction follows the initialization plan.
- `execution_tape` still sees ordinary object construction/runtime calls.
- Inspection must say user-defined `initialize` is deliberately deferred, not
  rejected as a direction.

### Steps

1. Add semantic `InitializationPlan`, plan-step, and init-argument records.
2. Refactor `make` validation so explicit args, defaults, required checks, and
   diagnostics are classified through the plan.
3. Add semantic tests for inherited order, explicit overrides, required
   inherited keywords, unknown/duplicate keywords, explicit/default type
   mismatch, and stable plan facts across trivia edits.
4. Commit the compiler/session model and validation cleanup.
5. Change lowered object construction to consume initialization-plan field order
   instead of iterating class effective slots ad hoc.
6. Add lowering/runtime tests for construction order and artifact changes after
   init metadata edits, then commit the lowering slice.
7. Add MOP/EOP/class inspection output for initialization plans, accepted
   keywords, defaults, required args, and override behavior.
8. Update docs and progress notes with the Dylan `make`/`initialize` boundary,
   then run final gates and examples.

### Risks

- The plan should not duplicate source slot facts so heavily that future
  metaclass work has two authorities.
- Diagnostics must stay specific enough for existing corpus expectations while
  separating unknown, duplicate, missing, explicit type, and default type
  failures.
- Dynamic class-object construction must still work when the class is statically
  known through `class <T>` values.

## Build Explainable Generic Dispatch-Site Inspection

### Goal

Make each semantic generic call inspectable as a dispatch-site artifact that
connects the call site, generic contract, argument facts, applicable/rejected
methods, lowered dispatcher order, and related query artifacts.

### Fence

The session/workbench layer owns dispatch-site inspection records and text
explanations. Semantic lowering still owns `GenericCall` resolution, and lowering
still owns executable dispatcher control flow. This slice does not add language
semantics, optimizer behavior, sealed dispatch, method combination,
multi-argument dispatch, or new execution_tape opcodes.

### Invariants

- A generic function is a metaobject.
- A method is a metaobject.
- A dispatch site is a call-site artifact, not a new language object.
- Runtime dispatch remains ordinary lowered control flow plus calls.
- Dispatch-site inspection must explain the current one-argument nominal model
  without implying broader dispatch features.
- Friendly IDs such as `dispatch:d0` are stable for unchanged call ordering;
  stable backing keys include source file, enclosing definition, owner-local
  call id, and generic definition, while records still report the current source
  span for navigation.

### Steps

1. Add public session dispatch-site records and explanation data by walking
   semantic functions/methods for `GenericCall` expressions.
2. Add specificity/applicability/rejection classification using existing
   nominal subclass helpers.
3. Add unit tests for exact selection, child-over-parent selection, parent/top
   runtime dispatch, sibling rejection, stable IDs across trivia, and changed
   method/class facts affecting explanations/artifacts.
4. Commit the compiler/session model slice.
5. Add `interloko_lab dispatch <file>` and `interloko_lab dispatch <file> <id>`, plus
   EOP `dispatch:dN` records and `inspect` support.
6. Add workbench fixtures/tests and commit the CLI/workbench slice.
7. Document how to list/inspect dispatch sites in MOP/EOP docs and progress
   notes, then run all final gates and examples.

### Risks

- IDs must be stable enough for harmless edits without overpromising stability
  after reordered or newly inserted call sites.
- Static selection must clearly distinguish exact/child-known calls from
  parent/top-object calls that need runtime dispatch.
- Explanations should name rejected methods without implying multi-method or
  method-combination semantics.

## Make Inheritance Semantically Real And Inspectable

### Goal

Make Interloko's existing single-inheritance class finalization affect semantic type
checks, runtime predicates/casts, generic dispatch, and workbench inspection,
without changing effective slot layout or adding inheritance-specific tape
operations.

### Fence

Semantic lowering owns nominal subtype facts and static compatibility checks.
Runtime layout and lowering consume those facts for existing predicates, casts,
slot access, calls, and dispatch. This slice does not own multiple inheritance,
method combination, sealed dispatch optimization, parametric types, host-backed
inheritance, or new execution_tape opcodes.

### Invariants

- `ast::Class::slots` remains source syntax.
- `sem::Class::direct_slots` remains resolved direct source facts.
- `sem::Class::effective_slots` remains finalized layout facts.
- Runtime layout still comes from effective slots.
- Subtyping is nominal and follows one superclass chain.
- Generic dispatch uses ordinary lowered control flow and calls.
- Host-backed classes remain non-constructible and not inheritance parents.

### Steps

1. Add semantic same-or-subclass helpers and regression tests for parent/child
   compatibility.
2. Teach static checks for slots, direct calls, returns, generics, casts,
   `instance?`, and restarts to use the helper where it is semantically valid.
3. Teach runtime predicates/casts/slot checks and lowered generic dispatch to
   use most-specific single-inheritance applicability.
4. Add unit, driver, corpus, and incremental coverage for the new subtype
   behavior and impossible sibling cases.
5. Commit the semantic/runtime/lowering slice.
6. Improve dumps for superclass chains, direct/effective/runtime slots, dispatch
   applicability, and inheritance-related artifact propagation.
7. Add workbench fixtures/tests and update docs/progress notes, then commit the
   inspection slice.
8. Run fmt, clippy, workspace tests, docs, corpus, and examples.

### Risks

- Static subtype acceptance must not accidentally allow arbitrary downcasts or
  unrelated classes.
- The dispatcher must pick the child method before a parent method without
  implying method combination.
- Dumps should expose useful propagation facts without creating a new CLI shape
  unless the existing commands cannot carry the information.

## Build AMOP-Informed Class Finalization And Inherited Slots

### Goal

Add a narrow, AMOP-informed class finalization slice: source classes may name a
single superclass, semantic lowering computes direct and effective slots, and
construction/reads/runtime layout consume the effective slot layout.

### Fence

AST and semantic lowering own class-source facts and computed finalization
facts. Runtime layout, lowered IR, tape emission, and EOP/MOP dumps are views or
consumers of those finalized facts. This slice does not implement a public AMOP
API, metaclasses, class redefinition, multiple inheritance, setters, host-backed
construction, persistence, or new tape opcodes.

### Invariants

- `<object>` remains the root/default superclass.
- Direct slots are source-authored facts with stable direct IDs.
- Effective slots are computed facts: parent effective slots first, then direct
  slots.
- Inherited effective slots preserve the original direct slot/accessor
  definition identity while receiving a deterministic layout position in the
  subclass.
- Construction and slot reads operate on effective slots.
- Dumps and EOP inspection make source facts vs computed facts explicit.
- Host-backed classes remain non-constructible and are not inheritance parents
  in this slice.

### Steps

1. Thread superclass syntax through AST and semantic class metadata.
2. Resolve and finalize native classes, computing effective slots and duplicate
   effective slot/init-keyword/accessor diagnostics.
3. Teach construction, accessor lookup, lowered IR, runtime layout, dumps, and
   EOP inspection to use finalized effective slots.
4. Add corpus, unit, incremental, and CLI coverage for inherited construction,
   inherited reads/accessors/defaults/required keywords, and error cases.
5. Add AMOP/finalization docs, update slot/progress notes, run all gates, and
   commit in coherent slices.

### Risks

- Effective slot IDs must be layout IDs, while definition/accessor identity
  stays tied to the source slot.
- Parent changes must affect child finalization without making child-only edits
  appear to mutate the parent.
- AMOP vocabulary must clarify architecture without implying CLOS/Dylan
  compatibility or an exposed metaclass protocol.

## Build Richer Dylan-Shaped Slots

### Goal

Extend slots from simple typed fields into explicit object-model facts with
construction keywords, default initializer expressions, and constant-slot
metadata, while keeping the existing construction and slot-read lowering path.

### Fence

Syntax/AST/semantic lowering own slot declaration meaning. Lowering/runtime own
the existing representation path for construction and reads. This slice
explicitly does not own setters, inherited slots, full Dylan initialization,
allocation variants beyond instance slots, persistence, or new tape opcodes.

### Invariants

- Untyped slots still normalize to `<object>`.
- Slot options are checked semantically before lowered IR is emitted.
- `constant slot` is metadata plus construction-time assignment only.
- Missing slot values are supplied only by explicit `init-value`; there is no
  implicit unbound value.
- Text dumps, EOP records, and workbench inspection are views over semantic
  slot facts.

### Steps

1. Extend syntax and AST for `constant slot` and `init-value: <expr>`.
2. Add semantic allocation/mutability/default metadata to slots.
3. Fill omitted construction fields from slot `init-value` expressions and
   type-check explicit/default initializers.
4. Update semantic, MOP, EOP, fingerprints, and workbench-visible inspection
   details.
5. Add corpus, unit, incremental, and CLI coverage for renamed keywords,
   defaults, required slots, object-top defaults, constant slots, and errors.
6. Update progress notes and run fmt, clippy, workspace tests, docs, examples,
   and corpus.

### Risks

- Default expressions should not imply a full init protocol or method hooks.
- Constant-slot metadata should not imply setter/mutation enforcement before
  setters exist.
- Slot fingerprinting must include defaults and metadata so incremental reports
  do not claim reuse after semantic slot changes.

## Make EOP Workbench Commands Usable

### Goal

Add a small read-only environment workbench surface to `interloko_lab`: inspect one
EOP object, list its affordances, invoke safe view-producing affordances, and
demonstrate explicit host projection through a tool fixture.

### Fence

`interloko_lab` owns interactive/workbench command glue over structured EOP records.
The compiler owns the records. Host adapters own explicit host projection.
This slice explicitly does not own Portolan subjects, Understory presentation,
persistence, automatic host reflection, runtime heap enumeration, mutation, or
authorization policy.

### Invariants

- Inspect/affordance commands read `EnvironmentObject` records, not text dumps.
- Affordance execution is read-only and maps to existing dumps/views.
- Host-projected records appear only when an explicit workbench provider is
  installed by fixture metadata.
- `InterlokoHost` remains an execution host, not an environment registry.
- Existing `eop` and corpus behavior stays intact.

### Steps

1. Add a `interloko_lab` workbench wrapper that compiles source and gathers
   compiler records plus optional host-projected records.
2. Add `inspect`, `affordances`, and read-only `invoke-affordance` commands.
3. Add explicit fixture metadata and a provider for host class-like and
   instance-like records.
4. Add CLI tests covering object kinds, affordance listing/execution, unknown
   IDs, and opt-in host projection.
5. Add usage docs and progress notes, then run fmt, clippy, tests, docs,
   examples, and corpus.

### Risks

- If command output becomes too clever, it will look like a UI contract instead
  of a workbench inspection tool.
- If fixture metadata is confused with Loko syntax, the language boundary gets
  muddy.
- If invoke support includes mutation affordances, this slice overreaches.

## Build Structured EOP Records and Host Projection Seam

### Goal

Turn the Environment Object Protocol from a text-only dump into typed compiler
records that tools can inspect directly. Keep the existing `interloko_lab eop` and
`interloko_corpus` behavior by rendering the text dump from those records.

### Fence

The compiler/session layer owns structured projection of compiler-known
environment facts. It explicitly does not own persistent world state, automatic
host discovery, runtime heap enumeration, Portolan subjects, Understory views,
or authorization policy.

### Invariants

- EOP records describe facts that already exist in the compiler, runtime layout,
  host metadata, diagnostics, and query graph.
- Text dumps are views over typed records; tests should not need to parse text
  to verify the protocol.
- `InterlokoHost` remains an execution host, not an environment registry.
- Host projection is explicit and adapter-owned.
- MOP answers object-system questions. EOP answers environment/tooling
  questions.

### Steps

1. Add public EOP record types, stable affordance names, a sink API, and session
   projection entry points.
2. Refactor the existing EOP dump to render from structured records while
   preserving CLI/corpus output.
3. Add an explicit host projection trait and a test provider that contributes
   host-owned environment objects without heap enumeration.
4. Add direct structured-record tests for native compiler records, host-backed
   compiler records, and host-projected records.
5. Update progress/design notes and run fmt, clippy, workspace tests, docs,
   `interloko_lab`, and `interloko_corpus`.

### Risks

- If host projection reaches into `InterlokoHost`, EOP becomes runtime enumeration
  rather than adapter-owned projection.
- If records imply a stable serialized schema, this slice will overpromise.
- If affordance names are just ad hoc strings, downstream tools will have to
  parse fragile text.

## Build Host-Backed Metaobject Foundation

### Goal

Introduce a narrow host-backed metaobject path so Rust-side adapters can
declare nominal Interloko classes and expose read-only slots without making Rust
objects Interloko-native heap objects. Host-backed classes should participate in
the same semantic class IDs, slot reads, `instance?`, `as`, and generic method
dispatch as native Interloko classes.

### Fence

Semantic IR owns nominal metaobject facts, class/slot origin, type categories,
and dispatch meaning. Runtime owns object handles, host-backed object identity,
and host-call execution. Adapter code owns Rust value access. Facet is deferred:
it may later populate the same registry, but it does not replace Interloko's MOP.

### Invariants

- The user-facing concept is host-backed, not foreign.
- Native and host-backed classes share the same `ClassId` space.
- Host-backed classes cannot be constructed by Loko `make` in this slice.
- Host-backed object handles are supplied by the embedding Rust host.
- Slot access is read-only and adapter-mediated.
- Lowered IR keeps class tests, casts, and slot reads as explicit model
  operations before tape emission.
- Tape emission uses ordinary host calls; no new tape opcodes.
- No `facet` dependency, automatic reflection, mutation, generic Rust types, or
  ownership/lifetime generality.

### Steps

1. Add semantic host-backed metaobject declarations:
   - `HostMetaRegistry`, `HostClass`, `HostSlot`, and IDs;
   - class/slot origin metadata on semantic classes and slots;
   - `sem::lower_with_host_registry` and compiler/session entry points that
     accept a registry.
2. Teach lowering and runtime layout about class/slot origin:
   - host-backed classes appear in dumps and runtime layout;
   - `make` rejects host-backed classes before lowering;
   - native class behavior stays unchanged.
3. Add runtime host object support:
   - public adapter trait for read-only Rust-side slot access;
   - public API to install a host-backed object handle in `InterlokoHost`;
   - `class_of`, `instance?`, `as`, slot reads, and `<object>` boxing work for
     host-backed handles.
4. Add execution/testing helpers:
   - compile with registry;
   - run named functions with explicit argument values in tests.
5. Add tests, corpus/fixtures where possible, dumps, fingerprints, and progress
   notes explaining what is real and how `facet` can later feed the registry.

### Risks

- If host-backed values become constructible too early, runtime ownership and
  lifetime policy will leak into the compiler.
- If registry metadata bypasses semantic class IDs, native and host-backed
  dispatch will split into two systems.
- Trait-object adapters must stay runtime-only so the core compiler remains
  no-std-friendly and dependency-light.

## Make Typed Object/Function Model Compose

### Goal

Add three narrow composition features as separate commits: typed generic
contracts, local object refinement, and statically known callable values. The
goal is to make existing class/object/function machinery compose without adding
closures, dynamic callable invocation, overloads, or new tape opcodes.

### Fence

AST owns only narrow surface syntax. Semantic lowering owns contract meaning,
local refinement state, known callable identity, and diagnostics. Lowering and
tape emission consume resolved semantic kinds and known call targets. Runtime
provides ordinary host support only where a dynamic class test/cast needs it; it
does not own static type checking.

### Invariants

- Existing examples and unannotated generic programs still compile.
- Direct-function type behavior remains unchanged.
- Generic contracts are collected before checking methods and call sites.
- Method return annotations are checked, and generic contracts constrain
  method result kinds when declared.
- `instance?` refinement is local to the true branch of the matching `if`.
- `<object>` values become useful through explicit tests/casts, not implicit
  unboxing.
- Known callable values stay statically known; no closures or unknown dynamic
  apply are introduced.
- Lowered IR and tape use existing calls, branches, and runtime calls.

### Steps

1. Typed generic contracts:
   - parse and AST-lower generic parameter/result annotations and method return
     annotations;
   - collect generic contract kinds before lowering methods and bodies;
   - check method specializers/results and generic-call arguments;
   - update dumps, fingerprints, tests, corpus, and progress notes.
2. Object tests, casts, and refinement:
   - add `instance?(x, <class>)` and `as(<class>, x)` semantic forms;
   - refine a matching local in the true branch of `if instance?(x, <class>)`;
   - lower through ordinary class checks, branches, traps, and runtime calls;
   - update dumps, fingerprints, tests, corpus, and progress notes.
3. Known callable objects:
   - add semantic value kinds/expressions for known direct functions, generic
     functions, classes, and accessors where practical;
   - add narrow `apply(f, arg...)` for statically known direct and generic
     function values;
   - lower known apply through existing direct/generic call paths;
   - update dumps, fingerprints, tests, corpus, and progress notes.

### Risks

- Generic contracts can look like a full type system. Keep them exact and
  local, with `<object>` as the only top boundary.
- Refinement needs to be visible in semantic dumps so it does not become hidden
  flow magic.
- Known callable values must not imply closures, captured environments, or
  late-bound invocation.
- `execution_tape` signatures remain static, so any runtime class-test/cast
  support must fit through existing host-call mechanisms.

## Add Typed Direct-Function Surface

### Goal

Add a narrow direct-function signature surface without changing generic or
method signatures. Function parameters may carry type annotations, function
returns may carry explicit result annotations, and direct calls are checked
against those signatures before lowering.

### Fence

The semantic layer owns direct-function signature meaning and call checking; the
AST owns narrow annotation syntax; lowering and tape emission consume resolved
value kinds and representation coercions only.

### Invariants

- Unannotated direct function parameters still default to `<integer>`.
- Unannotated direct function returns may remain inferred when available.
- Explicit parameter annotations are enforced at every direct call.
- Explicit return annotations are checked against the body result.
- Function signatures are collected before lowering function bodies.
- Class-object values stay distinct from runtime object instances.
- Generic declarations and method signatures do not gain new annotation syntax
  in this slice.

### Steps

1. Extend syntax and AST for direct-function parameter annotations and return
   annotations, including `class <name>` as the class-object type spelling.
2. Add semantic signature pre-pass data for parameter kinds and explicit return
   kinds.
3. Check direct-call arguments and annotated direct-function returns.
4. Teach lowering to coerce accepted top `<object>` parameters/returns through
   existing boxed-value representation.
5. Update dumps, fingerprints, corpus, and incremental tests for signature
   edits and signature-driven call invalidation.
6. Update progress notes and run fmt, clippy, workspace tests, docs, examples,
   `interloko_lab`, and `interloko_corpus`.

### Risks

- `<object>` accepts multiple concrete values semantically, so lowering must box
  direct-call arguments and returns at the call boundary.
- Explicit signatures collected before bodies must not erase useful inferred
  return kinds for earlier unannotated helper functions.
- Class-object type spelling should stay narrow so it does not imply
  metaclasses or reflective class protocols.

## Make Object Model More Dylan-Shaped

### Goal

Make two narrow object-model moves without implying a full Dylan object system:
slot reads become semantic accessor calls, and class names become semantic class
object values. Keep runtime lowering pragmatic: known accessors still become the
existing slot-read path, and known class objects still become class IDs.

### Non-Goals

- No metaclasses, user-defined classes of classes, or reflection.
- No setters, slot mutation, slot defaults, or full init protocol.
- No real inheritance semantics or parametric types.
- No first-class generic function values.
- No dynamic `make` for unknown class objects.
- No expanded effects/restarts or Cranelift.

### Fence

The semantic layer owns Dylan meaning. A slot access is a call through accessor
metadata, and a class name is a value. Lowering and runtime own representation:
known accessors become optimized slot reads, known class objects become class
IDs, and generic dispatch may keep using runtime class IDs internally.

### Steps

1. Add accessor metadata to slots with identity tied to the slot definition.
2. Rewrite semantic `c.radius` to accessor-call IR and add the same meaning for
   explicit `radius(c)` when it is unambiguous.
3. Lower accessor calls through the existing lowered `slot.read` instruction,
   update dumps, fingerprints, corpus, and incremental tests.
4. Make `<circle>` an AST and semantic class-object expression.
5. Change `make` to consume a known class-object expression and support simple
   local flow such as `let c = <circle>; make(c, radius: 10)`.
6. Add semantic `class-of(x)` returning a class-object value while lowering
   through existing class-id machinery.
7. Update progress notes and run fmt, clippy, workspace tests, docs, examples,
   `interloko_lab`, and `interloko_corpus`.

### Risks

- Accessor syntax must not shadow existing direct functions or generics.
- Class-object values should stay distinct from runtime instances even though
  known class objects lower to integer class IDs.
- The visible dumps need to show semantic meaning and representation lowering
  separately so this does not look more dynamic than it is.

## Make Slots Dylan-Shaped

Ticket: `cai-lnet`

### Goal

Make Interloko slot declarations follow the useful part of Dylan's slot shape:
untyped slots default to `<object>`, slots can declare required or renamed
construction keywords, and object construction reports focused diagnostics for
bad keyword initialization.

The intended surface shape is:

```text
define class <circle> (<object>)
  slot radius :: <integer>, required-init-keyword: radius:;
  slot cached :: <object>, init-keyword: cached-value:;
end;
```

The important semantic distinction is:

```text
<object>             => top Interloko value category
nominal <some-class> => runtime-managed object handle specialized to a class
```

### Non-Goals

- No default slot values, `init-value:`, or `init-function:`.
- No setters, `constant slot`, or setter naming.
- No `instance`, `class`, `each-subclass`, or `virtual` allocation variants.
- No inherited slot specifications.
- No parametric slot types.
- No full Dylan class initialization protocol.
- No sealed dispatch optimization or Cranelift.

### Fence

The compiler owns slot declaration metadata, top-value slot typing,
construction-keyword validation, semantic/lowered/tape artifact identity, and
dumps. The runtime owns carrying slot payload values through existing
host-call object operations. This slice does not own native aggregate layout,
boxed primitive identity, Dylan initialization methods, or optimization.

### Invariants

- Missing slot types resolve to the top `<object>` value category.
- `<object>` accepts integers, booleans, and nominal runtime object handles.
- Nominal class slot types still require a matching runtime object handle.
- A slot's construction keyword defaults to `slot-name:`.
- `init-keyword:` changes only the external construction keyword, not the slot
  read name.
- `required-init-keyword:` marks the slot as required and, when present,
  supplies the external construction keyword.
- In this slice, every slot still needs an initializer because defaults are not
  implemented; diagnostics should phrase required-keyword failures in keyword
  terms and ordinary missing initializers in slot terms.
- Duplicate, unknown, missing, and type-incompatible construction keywords
  produce diagnostics without panics.
- Lowering and tape emission continue to use ordinary calls and runtime host
  calls, not new generic or aggregate opcodes.

### Steps

1. Add parser and AST support for slot options:
   - `, init-keyword: keyword:`
   - `, required-init-keyword: keyword:`
2. Extend semantic slot metadata with declared type, effective value category,
   construction keyword, and requiredness.
3. Split value categories so top `<object>` is distinct from nominal class
   object handles.
4. Validate object construction by construction keyword, including duplicate
   keyword detection and focused missing/unknown/type diagnostics.
5. Update lowered IR and tape/runtime host-call signatures so top-value slots
   can store and return any first-slice Interloko value.
6. Update CST/AST/semantic/lowered/tape dumps, fingerprints, and artifact
   reporting to include declared type, keyword, requiredness, and runtime
   representation.
7. Add corpus fixtures for:
   - untyped `<object>` slots;
   - explicit primitive and nominal typed slots;
   - required init keywords;
   - renamed init keywords;
   - missing, unknown, duplicate, and type-incompatible construction keywords.
8. Add incremental tests proving slot metadata edits invalidate the affected
   class/layout/construction/lowered/tape artifacts without broad recompute.
9. Update progress notes with implemented semantics and deferred Dylan slot
   features, then run the full gates.

### Risks

- `execution_tape` host signatures are statically typed. Returning top
  `<object>` values requires a deliberate representation, likely a single
  runtime value handle for the first slice rather than pretending the value is a
  nominal object handle.
- Existing examples that relied on untyped integer slots should become explicit
  `:: <integer>` slots when their reads participate in arithmetic.
- The slot option syntax should stay narrow so it does not imply default values,
  setters, allocation variants, or inherited slot behavior.

## Build Richer Slot Type Model

Ticket: `cai-kf0d`

### Goal

Make class slot declarations carry a small explicit value kind and thread that
kind through object construction, slot reads, lowered IR, tape host signatures,
runtime object storage, dumps, corpus, and incremental reporting.

The intended surface shape is:

```text
define class <invalid-radius> (<object>)
  slot fallback :: <integer>;
  slot retry? :: <boolean>;
  slot cause :: <some-class>;
end;
```

This is a runtime-managed object model. It is not a general type system and it
does not add tape-native structs.

### Non-Goals

- No tape-native structs or aggregate opcodes.
- No sealed/value-class representation optimization.
- No inheritance-aware slot overriding.
- No parametric, union, intersection, nullable, or inferred types.
- No cross-function conditions, async, effect handlers, or Cranelift.

### Fence

The compiler owns slot declaration parsing, slot kind resolution, initializer
checking, and typed slot-read IR. The runtime owns storing and returning mixed
slot values for runtime-managed Interloko objects. Neither side owns native aggregate
layout in this slice.

### Invariants

- Untyped legacy `slot name;` declarations remained accepted in this slice.
  The later `cai-lnet` slot-initialization slice changed their default to
  `<object>`.
- Explicit primitive slot types are limited to `<integer>` and `<boolean>`.
- Explicit nominal slot types refer to already-declared or source-declared Interloko
  classes by name.
- `make(<class>, slot: value)` checks each initializer against the slot's
  declared value kind.
- Slot reads return the slot's declared value kind instead of a hard-coded
  integer.
- Tape emission continues to use host/runtime calls; only host signatures and
  runtime value storage become mixed-kind.

### Steps

1. Extend syntax and AST for optional `:: <type>` on slot declarations.
2. Add semantic slot kinds and resolve `<integer>`, `<boolean>`, and nominal
   object slot types while collecting classes.
3. Check object initializer kinds and make slot reads carry/read the declared
   kind.
4. Lower typed slot reads and update tape host signatures for per-class
   construction and per-slot read return types.
5. Store runtime object slots as `execution_tape::value::Value` so integers,
   booleans, and object handles round-trip through host calls.
6. Update AST/semantic/lowered/tape dumps, fingerprints, corpus fixtures, and
   incremental slot-type edit tests.
7. Update progress notes with the runtime-managed object ABI and deferred
   native-aggregate design, then run the full gates.

## Bind Restart Condition Objects

Ticket: `cai-240w`

### Goal

Let a lexical restart handler bind the signaled condition object and inspect its
ordinary nominal slots:

```text
handle
  signal(make(<invalid-radius>, fallback: 0))
with restart e :: <invalid-radius>
  e.fallback
end
```

This lifts the first restart slice's "no condition parameter" restriction
without adding a runtime handler stack or special execution_tape opcodes.

### Non-Goals

- No cross-call signaling.
- No dynamic handler lookup.
- No runtime condition stack.
- No multiple restarts or restart selection.
- No new condition declaration form.
- No new tape opcodes.

### Invariants

- The bound name is visible only inside the restart handler expression.
- The binding's semantic value category is the nominal condition object class.
- `signal` still evaluates the condition object once on the signal edge.
- Lowering moves that object into the handler-local register before jumping to
  the restart handler label.
- The existing unbound syntax remains accepted for simple handlers.

### Steps

1. Extend restart-clause syntax and AST with an optional `name ::` binding.
2. Bind the restart condition as a scoped semantic local for the handler body.
3. Lower the signal edge by moving the condition object into the handler-local
   register before the restart jump.
4. Update AST/semantic dumps, fingerprints, corpus fixtures, and incremental
   tests to show condition-slot access.
5. Record progress notes and run the full workspace gates.

## Build Recoverable Condition Restart Slice

Ticket: `cai-xvew`

### Goal

Add Interloko's first recoverable condition/restart vertical slice. Condition
objects remain ordinary nominal objects. `signal(condition)` inside a lexical
`handle ... with restart <condition-class> ... end` resumes with one handler
value, lowering to existing object construction, branches, jumps, and moves.

### Non-Goals

- No full effect typing.
- No stack inspection.
- No async.
- No dynamic extent edge cases.
- No multiple values.
- No optimizer work.
- No general cross-call unwinding.
- No runtime handler stack.

### Fence

This slice owns lexical condition signaling and one resumable restart value
inside a single function or method body. It explicitly does not own general
dynamic handler lookup, cross-call unwinding, typed effect inference, restart
selection, or runtime stack inspection.

### Syntax

The initial syntax was expression-oriented:

```text
handle BODY
with restart <condition-class>
  HANDLER
end
```

Signals use call-shaped syntax:

```text
signal(make(<condition-class>, fallback: 0))
```

### Plumbing

```text
ordinary class <invalid-radius>
  -> signal(make(<invalid-radius>, ...))
  -> handle body with restart <invalid-radius> handler end
  -> AST Signal / Handle / Restart
  -> semantic Signal / Handle / Restart + kind diagnostics
  -> lowered lexical restart frame:
       body normal path -> join
       signal path      -> restart handler -> join
  -> execution_tape ordinary object calls, branches, jumps, moves
```

### Invariants

- A `signal` must appear inside an active lexical `handle`.
- A `signal` condition must have a statically known nominal object class.
- The nearest active restart must specialize the signaled condition class.
- The handled body and restart handler must produce the same value category.
- The first slice did not bind the condition object. Ticket `cai-240w` adds the
  handler-local binding form.
- The core compiler crate remains `no_std + alloc`.

### Steps

1. Extend syntax for `handle`, `with`, and `restart` plus CST nodes for handle
   expressions and restart clauses.
2. Treat `signal(...)` as special call-shaped AST and add AST `Handle` and
   `Restart`.
3. Add semantic IR for `Signal`, `Handle`, and `Restart` with diagnostics for:
   - signal outside a handle;
   - signal condition that is not a known object;
   - signal class not handled by the active restart;
   - handled body / restart result kind mismatch.
4. Lower handles using an internal lexical restart frame in `BodyLowering`,
   without adding execution_tape opcodes or a runtime handler stack.
5. Update AST, semantic, lowered, and tape dumps/fingerprints.
6. Add corpus cases:
   - recovering from invalid radius;
   - checked arithmetic fallback;
   - diagnostics for uncaught signals and restart/body mismatches.
7. Add incremental tests proving restart handler edits recompute the edited
   method's lowered/tape artifacts while unrelated methods are reused.
8. Update progress notes and migration notes.
9. Run full gates:
   - `cargo fmt --all`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace --all-features`;
   - `cargo doc --workspace --all-features --no-deps`;
   - existing examples;
   - `interloko_lab` fixture tests;
   - `interloko_corpus`.

### Risks

- Lexical restart frames are intentionally smaller than real conditions. The
  docs must call out that cross-call signaling and handler stacks are deferred.
- `signal` returns the active restart's result category, so semantic lowering
  must lower restart handlers before the handled body.
- Nested handles should target the nearest matching active restart; corpus tests
  can stay small, but the lowering stack should be structured for nesting.

## Add Control Flow, Booleans, and Comparisons

Ticket: `cai-srg5`

### Goal

Add Interloko's first expression-level control-flow slice: boolean literals,
integer comparisons, and `if` expressions that flow through syntax, AST,
semantic IR, lowered IR, `execution_tape`, dumps, corpus fixtures, and
incremental cache reporting.

### Non-Goals

- No loops.
- No recursion-specific analysis.
- No short-circuit `and` / `or`.
- No pattern matching.
- No type inference beyond the current value categories.
- No parametric types.
- No effects or restarts.
- No sealed dispatch optimization.
- No Cranelift.

### Fence

This slice owns boolean/comparison/control-flow syntax and the direct lowering
needed to execute it. It explicitly does not own scheduler behavior, persistent
caches, richer type inference, multi-file module semantics, loop lowering, or
backend-specific control-flow opcodes.

### Plumbing

```text
source #t / #f / comparisons / if
  -> CST BoolExpr / BinaryExpr / IfExpr
  -> AST Bool / Compare / If
  -> semantic Bool value category + diagnostics
  -> lowered bool constants, comparisons, branches, moves, joins
  -> execution_tape ordinary instructions
  -> dumps, corpus, and incremental cache reports
```

### Invariants

- `if` conditions must have semantic kind `Bool`.
- Both `if` branches must produce the same semantic value category.
- Integer comparisons accept only `I64` operands and produce `Bool`.
- Arithmetic stays `I64 -> I64`; boolean support must not widen object slot
  storage or generic dispatch semantics.
- Lowered IR represents joins explicitly with labels, jumps, and register
  moves rather than a special high-level tape opcode.
- The compiler core remains `no_std + alloc`.

### Steps

1. Extend lexer/parser/CST for `#t`, `#f`, comparison tokens, and
   `if ... then ... else ... end`.
2. Extend AST lowering and dumps for bools, comparisons, and conditionals.
3. Extend semantic IR with `Bool`, comparison expressions, `if` expressions,
   and diagnostics for invalid conditions and mismatched branch kinds.
4. Extend lowered IR with bool constants, integer comparisons, moves, jumps,
   conditional branches, and explicit join values.
5. Emit the new lowered instructions into `execution_tape`.
6. Add corpus programs for passing control-flow examples and expected
   diagnostics.
7. Add incremental tests proving a small branch edit has focused lowered/tape
   cache effects.
8. Update progress notes with implemented semantics and deferred loop/recursion
   design.
9. Run full gates:
   - `cargo fmt --all`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace --all-features`;
   - `cargo doc --workspace --all-features --no-deps`;
   - existing examples;
   - `interloko_lab` fixture tests;
   - `interloko_corpus`.

### Risks

- Reusing `end` for both functions and `if` expressions can expose parser
  recovery bugs; tests should include nested conditionals.
- Direct-call return kinds are still simple and source-order based. This slice
  should document the limit rather than pretend full inference exists.
- Generic dispatch currently returns `I64`; conditionals may guard generic calls
  but should not expand generic return typing in this slice.

## Build Real In-Memory Incremental Compiler Engine

Ticket: `cai-e3zq`

### Goal

Turn the current one-shot session plus invalidation graph into a usable
in-memory incremental compiler engine. A caller should be able to keep named
source files in a session, edit them by stable `SourceFileId`, compile through
cached artifacts, and inspect which artifacts were reused, recomputed,
invalidated, and semantically affected.

### Non-Goals

- No persistent on-disk cache.
- No watch mode.
- No LSP.
- No parallel scheduler.
- No full multi-file module system.
- No new language semantics.
- No sealed dispatch optimization.
- No Cranelift.

### Fence

The incremental engine owns source-file edit state, artifact cache entries,
dependency-aware reuse/recompute reports, and per-artifact counters; it
explicitly does not own parser behavior, semantic rules, lowering rules,
runtime object storage, tape bytecode encoding, scheduling, or persistence.

The CLI tools own file IO, process exit behavior, and presentation; they
explicitly do not own cache dependency policy or artifact identity.

### Plumbing

```text
named source files
  -> IncrementalCompilerSession
  -> one-shot stage builders when a cache entry is missing/stale
  -> per-artifact fingerprints + dependency graph
  -> reuse / recompute / invalidated / affected reports
  -> interloko_lab edit output + interloko_corpus checks
```

### Invariants

- `SourceFileId` is stable for a logical file name or caller-provided id.
- `ArtifactKey` is the public identity for cache entries.
- A cache report must include every artifact known to the current query graph.
- Reuse is keyed by stable artifact fingerprints, not dense semantic indices.
- Dependency edges stay explicit and inspectable.
- The core compiler crate remains `no_std + alloc`.

### Metrics

Core cache reports use deterministic counters:

- `work_units`: estimated amount of artifact work performed for a recompute;
- `elapsed_ticks`: logical timing ticks for the artifact action.

This keeps the core no-std. A later scheduler or std wrapper can map the same
artifact reports to wall-clock timing without changing cache keys or dependency
semantics.

### Steps

1. Extend query artifact identity for diagnostics, lowered module, and runtime
   result artifacts.
2. Add an in-memory incremental session/cache API:
   - add named source files;
   - edit source by `SourceFileId`;
   - compile through cached artifact reports;
   - retain current `SessionArtifacts`.
3. Add artifact reports with:
   - action: reused or recomputed;
   - invalidated flag;
   - affected flag;
   - `work_units` and `elapsed_ticks`;
   - dependency edges.
4. Add tests for:
   - first compile populating cache;
   - repeat compile reusing all artifacts;
   - harmless trivia edits reusing most artifacts;
   - focused method body edits avoiding unrelated method artifacts;
   - definition-changing edits invalidating broad dependents.
5. Add `interloko_lab` output for edit reports.
6. Update `interloko_corpus` so corpus checks compile through the incremental
   engine.
7. Update progress notes and migration notes.
8. Run full gates:
   - `cargo fmt --all`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace --all-features`;
   - `cargo doc --workspace --all-features --no-deps`;
   - existing examples;
   - `interloko_lab` fixture commands;
   - `interloko_corpus` runner.

### Risks

- A whole-module compiler limits true partial artifact reuse. The first slice
  should make cache validity and reports explicit, while deferring a scheduler
  that can avoid all prerequisite recomputation.
- Dump-based or text-based fingerprints can be brittle. Prefer semantic
  fingerprints for method-level artifacts and normalize source-span-only text
  where possible.
- Public artifact enums will gain variants. Document this as a migration note
  in progress notes.

## Build Small-Program Corpus Runner

Ticket: `cai-5zew`

### Goal

Build a small, durable corpus of `.loko` programs and a workspace runner that
validates each program through the session API. The corpus should make it easy
to add new semantic examples, expected diagnostics, and lightweight dump
stability checks without committing large snapshot files.

### Non-Goals

- No new language semantics.
- No new production dependencies.
- No persistent snapshot store.
- No watch mode or editor integration.
- No replacement for `tools/interloko_lab`; this runner is batch validation, not an
  interactive workbench.

### Fence

The `corpus/` tree owns source fixtures and declarative expectations; it
explicitly does not own compiler behavior or dump formatting.

The `tools/interloko_corpus` crate owns filesystem traversal, expectation parsing,
session invocation, and human-readable check output; it explicitly does not own
syntax, semantic, lowering, tape, or runtime rules.

### Plumbing

```text
corpus/**/*.loko
  -> header expectations
  -> tools/interloko_corpus
  -> interloko_compiler::session
  -> runtime result / diagnostics / stage dumps
  -> pass/fail report
```

### Header Format

Each corpus file can declare expectations with leading comments:

```text
// expect: run 42
// expect: diagnostics 0
// expect: diagnostic contains unresolved local
// expect: dump lowered contains call
```

The runner treats these as the stable manifest for the file.

### Steps

1. Add `corpus/pass` and `corpus/fail` fixtures covering:
   - arithmetic;
   - locals;
   - direct calls;
   - classes, slots, and object construction;
   - generic dispatch;
   - expected diagnostics.
2. Add `tools/interloko_corpus` with:
   - recursive corpus discovery;
   - header expectation parsing;
   - session compilation;
   - runtime, diagnostic, and dump containment checks;
   - concise check and list commands.
3. Add tests for the runner and fixture set.
4. Update progress notes with corpus categories and how to add cases.
5. Run full gates:
   - `cargo fmt --all`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace --all-features`;
   - `cargo doc --workspace --all-features --no-deps`;
   - existing examples;
   - `interloko_lab` fixture commands;
   - `interloko_corpus` runner.
6. Close ticket and commit coherent changes.

### Risks

- Dump checks can become brittle. Use substring expectations that assert
  semantic landmarks rather than full dumps.
- Corpus expectations should not hide compiler regressions. Treat missing
  expectations as a runner failure.
- The core compiler remains `no_std + alloc`; keep all filesystem and process
  behavior in the tool crate.

## Build Compiler Session and Workbench CLI

Ticket: `cai-lk7f`

### Goal

Build a usable incremental compiler workbench around the existing Interloko
pipeline. The workbench should let a caller compile a named source file, inspect
every stage, list stable artifact keys, run `main`, and compare two file
versions for preserved/changed definitions and affected query artifacts.

### Non-Goals

- No watch mode.
- No LSP.
- No persistent on-disk cache.
- No full multi-file module system.
- No new language semantics.
- No sealed dispatch optimization.
- No parametric types.
- No effects/restarts.
- No Cranelift.

### Fence

The session layer owns source-file identity, per-stage artifact snapshots,
stable artifact-key reporting, dumps, and edit comparisons; it explicitly does
not own language semantics, lowering rules, runtime object storage, or
`execution_tape` bytecode internals.

The `tools/interloko_lab` crate owns file IO, CLI parsing, process exit behavior,
and presentation; it explicitly does not own compiler state or semantic
decisions.

### Plumbing

```text
.loko file(s)
  -> interloko_compiler::session
  -> syntax / ast / sem / lower / tape / runtime
  -> dumps + artifact graph + edit diff
  -> tools/interloko_lab CLI
```

### Steps

1. Add `session` API to `interloko_compiler`:
   - named source files with `SourceFileId`;
   - per-stage artifacts;
   - diagnostics aggregation;
   - optional runtime result;
   - artifact-key listing and artifact graph dump;
   - stage dumps.
2. Add structural dumps for CST, AST, and semantic IR.
3. Add session diff:
   - preserved definitions;
   - changed definitions;
   - affected query artifacts for focused method body edits.
4. Add `tools/interloko_lab` CLI:
   - `run <file>`;
   - `dump <file> --stage syntax|ast|sem|lowered|tape|all`;
   - `artifacts <file>`;
   - `diff <before> <after>`.
5. Add fixture programs and CLI/session tests for:
   - harmless edit preserving IDs;
   - focused method edit affecting expected artifacts;
   - CLI commands producing useful output.
6. Update progress notes and close ticket.
7. Run full gates:
   - `cargo fmt --all`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace --all-features`;
   - `cargo doc --workspace --all-features --no-deps`;
   - existing examples;
   - CLI fixture commands.

### Risks

- Adding dumps can bloat the core API. Keep dump methods simple text views and
  avoid adding dependencies.
- `invalidation 0.2` uses dense internal keys. Preserve public `ArtifactKey`
  reporting while keeping dense tracker indexes internal.
- The compiler crate is `no_std + alloc`. Keep filesystem and CLI concerns in
  `tools/interloko_lab`.
