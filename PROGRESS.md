# Interloko Progress

## Implemented: parse path uses the environment's compiled grammar

- `base_loko_primary::parse_file` now takes the compiled grammar as a
  parameter instead of compiling `BASE_LOKO_GRAMMAR_SOURCE` internally.
  Callers decide which grammar drives the parse.
- `grammar_frontend::parse_file_with_records` looks at
  `records.environment.slug` and routes through
  `compile_grammar_for_environment` to pick the right grammar:
  base-loko for everyone except the `derived-host` slug, which uses
  `compile_derived_host_grammar` (composes
  `derived_host.grammar` against the base via
  `try_compile_with_parent` at load time).
- Two new helpers in `grammar_frontend::interpreter`:
  `compile_base_loko_grammar` and `compile_derived_host_grammar` (plus
  the lower-level `compile_derived_host_grammar_with_parent` for call
  sites that already hold a base grammar). The
  reserved-word post-processing for the `name` rule is factored into
  `apply_loko_reserved_words` and shared between both helpers.
- `try_compile_with_parent` now runs on the hot parse path for
  derived-host, not only in a startup test. Inheritance is load-bearing
  in the parser, not just in the records layer.
- Removed the synthetic `function-definition` override that
  `derived_host_records` used to inject. The parser does not use that
  override (it compiles `derived_host.grammar`, which has no
  overrides), and matching applications against the synthetic record
  would attribute parsed function-definitions to a derived rule the
  compiler never ran. The records now reflect reality: zero direct
  rules, every effective rule resolves as `Inherited` from base. A
  new test
  `derived_host_parsed_function_definition_resolves_as_inherited`
  pins the records-vs-parser invariant by parsing a function under
  derived-host and checking the effective-rule resolution.

Deferred:

- Compiled grammars are not yet cached across calls. Each parse
  recompiles its grammar; the previous code did the same so this is
  not a regression, but a `OnceLock`-or-equivalent cache would help
  once profiling shows the cost. The kernel crate is `no_std + alloc`,
  so the cache primitive needs to stay no-std-clean.
- `derived_host.grammar` still has no rule overrides — derived-host
  parses the same surface language as base-loko. The natural follow-up
  is either a `super.<rule>` kernel feature so overrides can extend
  parent rules without duplication, or a duplication-style override
  that adds a derived-only form to demonstrate the path end-to-end.
- `compile_grammar_for_environment` is a hardcoded slug switch. If
  another environment provider lands, this is the next call site to
  generalize — likely by moving the compiled-grammar accessor onto
  `GrammarEnvironmentProvider` itself.

## Implemented: `derived_host.grammar` declares inheritance from source

- New `crates/interloko_compiler/grammars/derived_host.grammar` declares
  `grammar derived-host extends base-loko` with no rule overrides.
- `derived_host_records()` in `grammar_frontend.rs` now parses that file
  via `interloko_grammar::parse_grammar_source_full` and asserts the
  header's name and `extends` slug. The grammar source is the
  source-of-truth for the inheritance relationship; the Rust-side
  `GrammarId` mapping (`base-loko` → `BaseLokoGrammar`) stays in code.
- A new test `derived_host_grammar_source_compiles_against_base_loko`
  parses both `base_loko.grammar` and `derived_host.grammar` and runs
  `CompiledGrammar::try_compile_with_parent` end-to-end — the first
  production caller of the strict-compile API Phase 2 added.

Deferred:

- The hand-rolled `function-definition` override in
  `derived_host_records()` is still marked `TrustedHostFixture` and is
  not expressed in `derived_host.grammar`. Lifting it requires either
  a `super`-style parent-rule reference in grammar source (so the
  override can really delegate without recursing into itself) or
  wiring `parse_file_with_environment` to compile and parse with the
  environment's grammar (an architectural step distinct from this
  records-layer slice).

## Implemented: base-loko expression ladder uses direct LR

- `additive-expression`, `multiplicative-expression`, and
  `postfix-expression` in `base_loko.grammar` are now defined in
  direct left-recursive form, e.g.
  `additive-expression = additive-expression add-op multiplicative-expression
                       | multiplicative-expression`.
  The shim rules `additive-tail`, `multiplicative-tail`, and
  `slot-tail` are gone.
- `build_additive`, `build_multiplicative`, and `build_postfix` in
  `base_loko_primary.rs` walk the LR-shaped node directly: when the
  first child is the rule itself, build a `BinaryExpr` / `SlotExpr`
  from `(lhs_built_recursively, op_token, rhs)`; otherwise descend
  into the base alternative. The kernel produces the
  left-associative tree, so the builders no longer fold a tail list.
- A new corpus case `pass/arithmetic/left_associative_subtraction.loko`
  pins left-associativity for non-associative ops: `10 - 3 - 2`
  evaluates to `5`, not `9`.

Deferred:

- `evaluate_fold_binary` in `actions.rs` is wired to the calculator
  fixture grammar (`calculator.grammar`'s `fold-binary` action), not
  to base-loko. Migrating that grammar to direct LR would let the
  helper plus `emit-binary-tail` come out.

## Implemented: `list-of` / `trailing-list-of` and base-loko migration

- `base_loko.grammar` defines two parametric list helpers using the
  Phase-2 angle-bracket syntax: `list-of<elem, sep>` for strict
  separator semantics and `trailing-list-of<elem, sep> = list-of<elem,
  sep> sep?`. `trailing-list-of` is composed on top of `list-of`, so
  the strict form is load-bearing rather than dead code.
- `parameter-list`, `argument-list`, and `vector-items` are now
  one-liner invocations of `trailing-list-of<..., `,`>`. The
  `parameter-tail`, `argument-tail`, and `vector-tail` shim rules are
  gone.
- Loko now accepts trailing commas in parameter lists, argument
  lists, and vector literals. New corpus cases lock the laxness in:
  `pass/functions/trailing_comma_lists.loko` and
  `pass/values/trailing_comma_vector.loko`. Doubled commas remain a
  parse error, pinned by `fail/syntax/double_comma_argument_list.loko`
  and `fail/syntax/double_comma_vector.loko`.
- The CST builders `build_argument_list` and `build_vector` in
  `base_loko_primary.rs` collapsed to a single `filter` pass over
  the direct rule-child shape that the parametric form produces (no
  more `argument-tail` / `vector-tail` unwrap branches). A short
  comment in each builder names the kernel behavior that justifies
  the flat children: `Apply` substitutes the parametric body inline.

Deferred:

- The additive/multiplicative/postfix expression ladder still uses
  the `*-tail` shape. Moving it to direct LR is the natural pairing
  with the kernel's seed-then-grow support and removes
  `evaluate_fold_binary` plus the matching CST helpers.

## Implemented: Direct left recursion (seed-then-grow)

- `interpret_rule` now detects direct left recursion in a rule body
  (`R = R alt | ... | base`) and uses the classical PEG seed-then-grow
  technique to match it: seed with the non-LR alternatives, then
  iteratively wrap the current match with each LR alternative's tail
  while a tail extends. The construction is left-associative.
- Detection is syntactic: `direct_lr_partitions` examines a rule's
  top-level `RuleExpression::Choice` and partitions alternatives into
  LR (start with `Rule(self)`) and non-LR. Rules without that shape
  use the existing recursive descent path with no overhead.
- No memo table is added. Indirect left recursion (a chain through
  multiple rules) is not handled here; rules that need it should be
  refactored into a single direct-LR form, or the grammar can keep
  using the existing tail-rule ladder.
- The `excluded_text` check applies to the final grown span, matching
  the non-LR rule contract.
- Tests cover: left-associative `1+2+3`, fall-through when the seed
  fails, and stop-on-tail-failure when growth can't extend.

Deferred:

- Rewriting `base_loko.grammar`'s `additive-expression`,
  `multiplicative-expression`, and `postfix-expression` to direct-LR
  form (collapsing the `*-tail` rules) requires updating the CST
  builders `build_additive`, `build_multiplicative`, `build_postfix`
  and removing `evaluate_fold_binary` in
  `interloko_compiler/src/grammar_frontend/actions.rs`. The kernel
  feature is in place; the migration is a focused follow-up.
- Indirect left recursion (cross-rule chains) needs a memoization
  table à la Warth; the seed-then-grow technique here is the
  syntactic-direct-LR subset.

## Implemented: Grammar inheritance (`extends`)

- `parse_grammar_source` now captures the `grammar X` header line and a
  trailing `extends Y` clause. New public surface:
  `GrammarSourceHeader { name, extends }` and `ParsedGrammarSource`
  bundle plus `parse_grammar_source_full` entry point. The existing
  `parse_grammar_source_with_diagnostics` keeps its return shape via
  a small `parse_grammar_source_inner` helper that everyone delegates
  to. The first header wins; further `grammar` lines are ignored.
- `CompiledGrammar::compile_with_parent(rules, parent)` builds an
  inheritance-aware grammar: child rules take precedence over
  parent rules with the same name; un-overridden parent rules are
  flattened into the child for lookup. The existing
  `CompiledGrammar::compile` is unchanged.
- Tests cover header parsing (with and without `extends`), override
  precedence in `compile_with_parent`, and child rule bodies that
  reference parent-only rules through the chain.

Deferred:

- Lifting `DerivedHostEnvironmentProvider` in
  `interloko_compiler/src/grammar_frontend.rs` to read from an actual
  `derived_host.grammar` file. The existing fixture's override rule
  for `function-definition` references itself as `Rule("function-definition")`,
  which under real inheritance semantics would left-recurse. The
  fixture is currently MOP-only metadata; migrating it requires
  rewriting the override to a non-self-referential body. Tracked as
  the next thing to do once the trusted fixture's semantics are
  worked out.
- A `provenance: GrammarId` field on `StagedRuleApplication` /
  `StagedRuleCount` so logged rule applications can distinguish a
  child override from an inherited parent rule. For now the staged
  records carry effective rule names (child wins) and that's the
  documented behavior.

## Implemented: Parametric rules (`list-of<elem, sep>`)

- `RuleExpression` gains an `Apply { rule, args }` variant. Grammar
  surface: `rule-name<arg, arg, ...>` with the angle bracket glued to
  the rule name. `SourceAuthoredRule` gains a `params: Vec<&'g str>`
  field — populated when a rule header declares formal parameters
  (`rule list-of<elem, sep> = ...`).
- The matcher's `Apply` arm looks up the target rule, substitutes
  each parameter reference in the body with the corresponding
  argument expression, and interprets the substituted form.
  Substitution is match-time and walks the expression tree once per
  invocation; nested parametric calls and self-referential parametric
  definitions work without a separate compile pass.
- Parsing changes: `extract_rule_name_header` strips `<...>` formal
  params from the rule name; `parse_apply_term` recognizes invocation
  shape after the other term-prefix/suffix checks; `split_top_level`,
  `split_whitespace_top_level`, and `split_transform_top_level` now
  track angle-bracket depth so commas, whitespace, and `=>` inside
  `<...>` don't split.
- Display round-trips `list-of<digit, `,`>` shape.
- Downstream `rule_expression_source_data` gets an `apply` arm with
  rule name and arg list.
- Tests cover parametric definition recording params, invocation
  matching with substitution, Display round-trip, and composition
  with named subexpressions.

Deferred:

- Rewriting `base_loko.grammar`'s `parameter-list`, `argument-list`,
  and `vector-items` in terms of `list-of` requires updating the CST
  builders in `interloko_compiler/src/grammar_frontend/base_loko_primary.rs`
  (build_parameter_list, build_argument_list, build_vector) to handle
  the flattened child shape (no more `*-tail` wrapping nodes). That
  CST migration is a follow-up — the kernel feature is in place.

## Implemented: Named subexpressions (`name:expr`)

- `RuleExpression` gains a `Named { name, inner }` variant.
  Grammar surface: `name:expr` (glued colon, outside backticks). The
  matcher tags every child produced by the inner expression with the
  binding `name` so actions can resolve a specific child by binding
  rather than by rule type.
- `RuleNode` is now `#[non_exhaustive]` with a new
  `binding_name: Option<&'g str>` field. New helper
  `RuleNode::find_by_binding(name)` returns the first direct child
  whose binding matches.
- `GrammarActionTerm` gains `CaptureByName { name }`. Bare identifiers
  in action arguments now resolve to `CaptureByName` when the
  enclosing rule defines a matching binding; otherwise they remain
  `Capture { rule }`. The binding set is collected from the rule's
  expression and threaded into action parsing via the new
  `parse_grammar_action_with_bindings`.
- `parse_rule_term` recognizes the named prefix before the suffix
  operators, so `lhs:name?` parses as `lhs:(name?)` (a bound optional)
  and `lhs:`x`` produces a bound literal.
- Display round-trips `name:expr` and treats `CaptureByName` as a
  bare identifier (which becomes ambiguous with `Capture` only when
  re-parsed without binding context — documented).
- Downstream `rule_expression_source_data` gains a
  `named-subexpression` arm; `grammar_action_term_source_data` gains
  `capture-by-name`.
- Tests cover: two same-named children disambiguated by binding,
  action terms resolving to `CaptureByName` vs falling back to
  `Capture`, and Display round-trip.

Deferred:

- A binding on a sequence inner (`lhs:(a b)`) tags every child of the
  sequence — actions that expect a single node should use a
  rule-reference inner. Document the multi-child case in evaluator
  contracts when a real consumer arrives.

## Implemented: Richer action terms (Integer, List, Record, Call)

- `GrammarActionTerm` gains `Integer(i64)`, `List(Vec<...>)`,
  `Record(Vec<GrammarActionArg<'g>>)`, and `Call(Box<GrammarAction<'g>>)`.
  The kernel still treats action descriptors as inert data; the new
  variants let grammar authors capture structured projections in the
  descriptor instead of pushing all the shape into the evaluator.
- Surface syntax: bare integers (`42`, `-1`), `[term, term]` lists,
  `{ key: term, key: term }` records, and nested `name(arg: term)`
  calls. `parse_grammar_action_term` recognizes each form and
  `split_action_args` now tracks `()`, `[]`, `{}` depth in addition
  to the existing `"`/`` ` `` string/literal tracking, so commas
  inside nested terms don't split.
- Display round-trips each new variant. Downstream
  `grammar_action_term_source_data` in
  `interloko_compiler::grammar_frontend` gains explicit `integer`,
  `list`, `record`, and `call` projection arms; the `_ =>
  unknown-grammar-action-term` catch-all stays as forward-looking
  insurance.
- Tests cover integer (positive and negative), list, record, nested
  call, and a full Display → parse round trip.

Deferred:

- The action evaluator (`grammar_frontend/actions.rs`) doesn't yet
  consume the new variants — that's part of Phase 3 projection
  work. Current evaluator paths still operate on Capture / CaptureAll
  / Literal / Bool only.

## Implemented: Compiler grammar_frontend uses CompiledGrammar

- `interloko_compiler::grammar_frontend::interpreter::parse_source_authored_grammar`
  now returns a `CompiledGrammar<'static>` (with the existing reserved-word
  policy applied during build). The downstream `interpret_region_result`
  and `interpret_rule_error` helpers take `&CompiledGrammar` and dispatch
  to the kernel's `interpret_region_compiled` /
  `interpret_rule_error_compiled` entry points.
- `base_loko_primary.rs`, `calculator.rs`, and `records.rs` parse via the
  compiled grammar; the staged-rule projection paths (`rules(...)`) clone
  through `grammar.rules()` to keep producing the existing `Vec<GrammarRule>`
  output without owning the kernel-side rule list.
- The grammar_frontend inline tests pass `&rules` (now `&CompiledGrammar`)
  unchanged; one site that indexed the slice (`rules[0].expression`) is
  now `rules.rules()[0].expression`.
- Kernel-side rule lookups during parsing now use the compiled binary
  search instead of the linear scan. The action-evaluator helpers in
  `actions.rs` still take a slice and continue with their existing linear
  pattern.

Deferred:

- Migrating `evaluate_rule_action` and helpers in `actions.rs` to take
  `&CompiledGrammar` and use `grammar.rule(name)`. Their scans only fire
  once per region, not on the parse hot path, and the broader rewrite is
  best paired with the Phase 2 action vocabulary work.

## Implemented: Tighten string-literal terminator and drop compensating workarounds

- `base_loko.grammar` `string-literal` now requires the closing `"` instead
  of leaving it optional. Unterminated strings fail in the kernel directly
  with `expected `"``-shape parse error.
- `grammar_frontend/base_loko_primary.rs` loses `malformed_string_span` (the
  re-lex workaround that produced `ast.malformed-string-literal`) and the
  `let-statement` retry branch in `base_loko_parse_error` (along with the
  `nearby_token_start` helper). Unterminated strings now surface as the
  natural `syntax.expected-token` failure with `expected = ``end```; the
  parser backtracks past the failed string-literal and reports the next
  required literal of the enclosing rule.
- Corpus expectation in `corpus/fail/values/malformed_string.loko` updated
  from `ast.malformed-string-literal` to `syntax.expected-token`. The
  `reports_malformed_strings_and_unsupported_vector_equality` driver test
  follows the same change.
- `DiagnosticCode::AstMalformedStringLiteral` is retained because
  `ast.rs::lower_string` still emits it when `decode_string_literal` rejects
  a well-formed-but-undecodable string body.

Deferred:

- The kernel's error span for this case lands on the byte after `()` (where
  the next literal would start), not on the unterminated `"`. A more
  user-friendly diagnostic would require either reviving an unterminated-
  string heuristic or improving the kernel's failure-reporting strategy to
  surface deeper Choice-merged failures past Optional boundaries.

## Implemented: Per-rule trivia mode (`raw`)

- New public `TriviaMode` enum (`Default` / `Raw`, `#[non_exhaustive]`)
  plus a `trivia` field on `SourceAuthoredRule`. Grammar surface: an
  optional `raw` keyword between the rule name and `=`, e.g.
  `rule string-literal raw = `"` until(`"`) `"`?`. The previous rule
  shape continues to parse as `TriviaMode::Default`.
- Raw rules skip neither leading nor inter-literal trivia. Rule references
  inside a raw rule still follow the called rule's own mode, so a raw
  rule embedding a `Default` rule still gets the called rule's trivia.
- Unknown modifiers between the name and `=` produce a new
  `GrammarSourceDiagnosticCode::UnknownRuleModifier` and the rule parses
  with `TriviaMode::Default`.
- `interpret_rule` and `interpret_expression` now thread a `raw: bool`;
  `RuleExpression::Literal` consults it before deciding whether to skip
  trivia.
- `interloko_compiler/grammars/base_loko.grammar` opts `string-literal`
  into raw mode. Full corpus (138 cases) still passes.
- Tests cover raw-rule whitespace rejection, default-mode trivia skip
  for comparison, the string-literal example, and the unknown-modifier
  diagnostic.

Deferred:

- Dropping the optional `?` on the closing `"` in `string-literal` so
  unterminated strings become real parse errors. Once that's in, the
  `malformed_string_span` workaround in
  `interloko_compiler::grammar_frontend::base_loko_primary` can go.
- Revisiting the `interpret_rule_error` retry chain in
  `base_loko_parse_error` now that lookahead, cut, and raw mode are
  available.

## Implemented: Committed-choice cut (`^expr`)

- New `RuleExpression::Cut` variant. Grammar surface: prefix `^item`,
  parsed in the same band as `&` / `!`. Once the inner expression of a
  cut matches inside an alternative of an enclosing `|` choice, sibling
  alternatives are no longer tried; a subsequent failure in the same
  alternative becomes the choice's failure verbatim.
- Internal `ExpressionMatch` and `ParseFailure` carry a private
  `committed` flag. `Cut` raises it on success; `Sequence`, `Optional`,
  `OneOrMore`, `ZeroOrMore` propagate it; `Choice` clears it on success
  and on the committed-failure abort path (so an outer choice can still
  try its alternatives). Rule references drop the flag at the rule
  boundary, so cut never bubbles across rules.
- Downstream `grammar_frontend.rs::rule_expression_source_data` gains a
  `cut` projection arm.
- Tests cover: cut prevents fallthrough after match (and absent cut
  still falls through), cut inside Optional does not bubble, and Display
  round-trip for the new prefix.

Deferred:

- Surfacing committed-failure spans through `interpret_rule_error`
  callers as recovery anchors — Phase 3 territory.

## Implemented: Positive and negative lookahead

- New `RuleExpression::And` and `RuleExpression::Not` variants for positive
  and negative lookahead. Grammar surface: prefix `&item` and `!item`.
  Prefix binds looser than the trailing `*`/`+`/`?` operators so `&item*`
  is `&(item*)`. Whitespace between prefix and operand is not allowed.
- New `ExpectedFragment::NegativeLookahead` is reported when a negative
  lookahead match prevents the rule from consuming.
- Lookahead leaves no children on the parse tree; the matcher position is
  unchanged on success.
- Negative lookahead deliberately discards the inner expected-fragments —
  they would mislead the user.
- `parse_rule_term` parses the prefix before the suffix operators.
- `interloko_compiler::grammar_frontend::rule_expression_source_data` gets
  `positive-lookahead` / `negative-lookahead` projection arms.
- Tests cover positive lookahead non-consumption, negative lookahead
  rejection with the new fragment, non-consumption combined with following
  expressions, and Display round-trip including parenthesization.

Deferred:

- Lookahead-aware error-recovery synchronization — Phase 3.

## Implemented: Richer character classes

- `character_class_contains` is now a zero-allocation state machine that
  walks the class body once. New syntax inside `[...]`:
  - Leading `^` negates the class (`[^a-z]`).
  - Backslash escapes for metachars and whitespace controls (`\\`, `\]`,
    `\[`, `\^`, `\-`, `\n`, `\r`, `\t`).
  - ASCII shorthand classes `\d`, `\D`, `\w`, `\W`, `\s`, `\S` with the
    expected complements.
- Byte-for-byte preserves existing grammars: `[A-Za-z]`, `[A-Za-z_]`,
  `[0-9]`, `[$A-Za-z_]`, and `[A-Za-z0-9_?-]` (trailing `-` stays literal
  because the parser only forms a range when both endpoints are present
  and the end isn't a backslash).
- Tests cover the trailing-dash pitfall explicitly, negation, escapes,
  shorthand classes, and the four `base_loko`/`records`/`calculator`
  class shapes already in use.
- README's "Supported expression forms" section is updated to document
  the new syntax.

Deferred:

- Unicode-category classes (`\p{...}`) and non-ASCII shorthands — Phase 2
  territory.

## Implemented: CompiledGrammar with binary-search rule lookup

- New `CompiledGrammar<'g>` owns a rule list and a sorted-by-name index.
  `compile`, `rules`, and `rule(name)` are public; the `*_compiled` matching
  entry points (`interpret_region_compiled`, `inspect_region_compiled`,
  `interpret_rule_error_compiled`) accept it and do binary-search rule
  lookups during matching.
- Existing slice-taking `interpret_region` / `inspect_region` /
  `interpret_rule_error` keep behavior and now share a private `RuleLookup`
  helper with the compiled paths. Slice users keep the linear scan they
  always had; callers wanting O(log n) lookup migrate to a pre-built
  `CompiledGrammar`.
- Lookup preserves first-wins behavior on ties (when a caller manually
  constructs duplicate rule names) so the slice and compiled paths report
  identical results.
- Tests verify parity across success, failure, trailing-input inspection,
  and `interpret_rule_error` between the slice and compiled paths.

Deferred:

- Switching `interloko_compiler::grammar_frontend` to `CompiledGrammar` —
  the kernel exposes the path; compiler adoption can land separately.
- Caching compiled grammars across compiler sessions.

## Implemented: Tree geometry and span helpers

- `Span` gains `cover` and `intersect`, returning the smallest span covering
  two spans and the half-open overlap respectively. Both are `const fn`.
- `RuleNode` gains `descendants_in_span(span)` (preorder list of contained
  nodes, including self when applicable) and `path_to_offset(offset)`
  (root-first path of nodes covering an offset, deepest last).
- New free function `line_column_at_byte_offset(source, offset)` returns
  1-based line and byte-column coordinates. Columns count bytes, matching
  characters for ASCII grammars; offsets past the end clamp to the last
  line's end.
- Tests cover the cover/intersect edge cases (touching half-open spans do
  not overlap), descendants restricted to a sub-span, the path walk down
  through a `name` rule, and line/column edge cases including clamping.

Deferred:

- Unicode-aware column counting and lossless CST shapes — Phase 3 territory.

## Implemented: Mark kernel enums and SourceAuthoredRule non-exhaustive

- `RuleExpression`, `TransformOutput`, `ExpectedFragment`, `RuleNodeKind`,
  `RuleNodeRole`, `GrammarActionTerm`, `GrammarSourceDiagnosticCode`, and the
  struct `SourceAuthoredRule` are now `#[non_exhaustive]`. Future kernel
  commits can add variants (lookahead, cut) or per-rule policy fields (trivia
  mode) without forcing breaking changes on downstream pattern matches.
- Downstream catch-alls added at the five exhaustive match sites in
  `interloko_compiler` (`grammar_frontend.rs` rule-expression / transform /
  action-term projections; `session.rs` explain/label helpers). Each falls
  through to `unreachable!()` so a future variant addition produces a clear
  panic if a projection arm is forgotten.

Deferred:

- Marking less load-bearing types like `RuleCount`, `Parse`, `ParseError`,
  `RuleNode` non-exhaustive — Phase 1 keeps the change targeted.

## Implemented: Grammar-source diagnostics

- `interloko_grammar` now reports structural problems in grammar source instead
  of silently dropping lines. `parse_grammar_source_with_diagnostics` returns
  rules alongside a list of `GrammarSourceDiagnostic` values; the existing
  `parse_grammar_source` stays as a shim that discards diagnostics.
- New public surface: `GrammarSourceDiagnostic`, `GrammarSourceDiagnosticCode`
  with stable codes `missing-rule-keyword`, `missing-equals`, `empty-name`,
  `empty-body`, `duplicate-name`. Each code has a static `as_str()` and
  `message()`.
- Duplicate rule names preserve "first wins" lookup semantics, matching the
  existing `rules.iter().find` behavior; subsequent declarations are reported
  and skipped.
- Tests cover each diagnostic code, the legacy shim, and a regression for
  rule bodies that contain a literal `=` (e.g. `rule equals = `=``).

Deferred:

- Surfacing grammar-source diagnostics through `interloko_compiler` — Phase 1
  keeps the kernel boundary clean.
- Diagnostics for unknown rule modifiers — added in the per-rule trivia
  commit.

## Implemented: Top-Level Constants

- Loko now accepts immutable top-level constants using the Dylan naming
  convention: `define constant $name :: <type> = expression;`.
- Constants are parsed as top-level source forms, receive stable
  `DefinitionKind::Constant` identities, and lower through CST, AST, semantic
  IR, lowered IR, and tape emission like other expression-bearing definitions.
- Semantic binding records constants as immutable named values. Functions,
  methods, and later expressions can reference constants with the ordinary name
  lookup path. Duplicate names and unresolved references remain structured
  diagnostics.
- Constant initializers must produce runtime values. Function/generic/accessor
  names are rejected as initializer values with
  `sem.constant-initializer-type-mismatch` rather than becoming hidden callable
  globals.
- Lowering inlines the initializer expression through the existing expression
  pipeline. No new global-storage opcode, module-initialization tape, mutable
  global state, or constant-folding pass is introduced in this slice.
- Constants are projected as EOP/MOP objects such as `constant:k0`, carry
  source and artifact facets, appear in source-form `defines` output, and are
  addressable by REPL/lab lookup surfaces.
- Corpus and driver coverage includes integer, boolean, string, class-object,
  and native object constants, plus duplicate, unresolved, and invalid
  initializer diagnostics.

Deferred:

- Mutable top-level variables.
- Module/import/export scoping.
- A dependency scheduler for forward or cyclic constant initializers.
- Persistent image semantics for object constants.
- Optimizer-level constant folding or sealed static propagation.

## Implemented: Binding Type Annotations

- Constants and local `let` bindings now use the same `::` type annotation
  notation as parameters, results, and slots:
  `define constant $answer :: <integer> = 42;` and
  `let radius :: <integer> = 10;`.
- AST preserves optional `TypeAnnotation` records for constants and local
  bindings. Semantic lowering resolves those annotations through the same class
  and built-in value type rules used by function signatures.
- Annotated constants and locals are checked against their initializer
  expression. Mismatches report structured diagnostics:
  `sem.constant-type-mismatch` and `sem.local-type-mismatch`.
- Lowering respects the semantic local kind, so annotated locals use the same
  coercion path as function results and other checked expression boundaries.

Deferred:

- Mutable top-level variables.
- Type aliases, union types, parametric types, and refinements.
- Flow-sensitive reinterpretation of local annotations after assignment-like
  forms.
- A dependency scheduler for forward annotated constants.

## Implemented: Save And Open Session Transcripts

- A new `interloko_compiler::transcript` module (gated behind the
  existing `std` feature) makes the session mutation log durable.
  `SessionArtifacts::to_transcript()` captures the live session as a
  `SessionTranscript { format_version, source_file_name, base_source,
  next_mutation_id, events }`. `SessionTranscript::save(dir)` writes
  the on-disk layout; `SessionTranscript::open(dir)` reads it back;
  `SessionTranscript::restore(grammar_registry, host_registry)`
  rehydrates a live `SessionArtifacts` by compiling the base source
  and replaying the events through the existing strict-replay
  machinery.
- The on-disk layout is a structured directory:
  `transcript.toml` + `base.loko` + `mutations/NNNNNN-add-method.loko`
  files. The manifest is TOML, human-readable, line-diffable, and
  carries one `[[events]]` table per mutation. The
  `NNNNNN-<kind>.loko` filename pattern preserves stable session
  event ids and any gaps left by rewind.
- The transcript records history, not a snapshot of derived runtime
  state. No cached artifacts, no lowered IR, no tape, no runtime
  trace, no diagnostic state, no host registry contents. The live
  image is reconstructed via replay at restore time; only the
  transcript is durable.
- Strict replay only on restore. The recorded `generic_id_hint` is
  used verbatim as the request's `GenericFunctionId`. The
  `generic_name` field is recorded for human inspection and future
  conflict-handling work, but this slice does not silently rebind by
  name when ids drift between save and open; restore surfaces the
  standard `ReplayFailure` taxonomy instead.
- Rewind high-water mark is preserved across save / open / restore.
  The transcript records `next_mutation_id` explicitly; the
  ephemeral restored session sets its counter to that value, not to
  `max(applied) + 1`. Dropped ids stay retired across persistence.
- Errors are structured. Save returns `TranscriptSaveError {Io,
  Serialize}`. Open returns `TranscriptLoadError {Io, Manifest,
  UnsupportedFormatVersion, UnknownEventKind, MissingEventSource,
  InvalidEventSourcePath, EventIdsNotStrictlyIncreasing,
  NextMutationIdNotAboveMaxEvent}`.
- `open` validates the manifest before touching the filesystem for
  any event source. A hand-edited or hostile manifest cannot make
  `open` read outside the transcript directory (each event's
  `source` must equal the canonical
  `mutations/<NNNNNN>-<kind>.loko` path), recycle ids (events must
  be strictly increasing in `mutation_id`), roll the id counter
  backward (`next_mutation_id` must be `> max(event_id)`), or
  claim a mismatched schema version (`format_version` must equal
  `FORMAT_VERSION` exactly — both newer and older are rejected
  until migrations exist).
- Internal plumbing: `apply_session_mutation_with_id` and
  `compile_file_with_registries` gain `pub(crate)` visibility so the
  transcript module can reuse the apply-with-explicit-id and compile
  paths without duplicating them. `SessionArtifacts` gains public
  accessors `pristine_source()` and `next_mutation_id()` and a
  `pub(crate)` `set_next_mutation_id` setter for the restore path.
- Optional dependencies `toml` and `serde` are added behind the
  `std` feature; `tempfile` is a new dev-dependency for disk
  round-trip tests.
- Sixteen tests pin the contract:
  - in-memory round trip (`to_transcript + save + open` is
    byte-equal);
  - dispatch reproducibility (`save + open + restore` produces the
    same dispatch behavior as the original session);
  - id-gap preservation (apply, apply, rewind, apply -> log `[0, 2]`,
    `next_mutation_id = 3` all survive the round trip);
  - high-water mark preservation when `max(applied) + 1` is lower
    (apply, apply, rewind to first -> log `[0]`, `next_mutation_id =
    2`; restored session keeps `next = 2` so dropped id 1 stays
    retired);
  - restore against a base modified to remove the target generic
    surfaces a structured `ReplayFailure`;
  - on-disk layout discipline (the directory contains exactly
    `transcript.toml`, `base.loko`, and the `mutations/` subdirectory
    with one `NNNNNN-add-method.loko` file per event; no cached
    artifacts, no runtime state, no tape);
  - malformed manifest surfaces `TranscriptLoadError::Manifest`;
  - deleting an event source file surfaces
    `TranscriptLoadError::MissingEventSource` with the right
    `mutation_id` and path;
  - manifest claiming an unknown event kind surfaces
    `TranscriptLoadError::UnknownEventKind`;
  - manifest event source containing `..` is rejected with
    `TranscriptLoadError::InvalidEventSourcePath`;
  - absolute manifest event source path is rejected with
    `TranscriptLoadError::InvalidEventSourcePath`;
  - duplicate `mutation_id` in events is rejected with
    `TranscriptLoadError::EventIdsNotStrictlyIncreasing`;
  - out-of-order `mutation_id` in events is rejected with
    `TranscriptLoadError::EventIdsNotStrictlyIncreasing`;
  - `next_mutation_id <= max(event_id)` is rejected with
    `TranscriptLoadError::NextMutationIdNotAboveMaxEvent`;
  - `format_version = 0` is rejected with
    `TranscriptLoadError::UnsupportedFormatVersion`;
  - the two-pass discipline holds: a structural error on a later
    event surfaces without triggering a file-read attempt on any
    earlier event.
- All 179 pre-existing tests continue to pass against the relaxed
  visibility of the apply / compile internals.
- See [`docs/eop_mutation.md`](docs/eop_mutation.md) for the
  transcript contract, the directory layout, the strict-replay-only
  rule, and the still-deferred follow-ups (branch, checkpoints,
  schema migration tooling, rebind-by-name, in-language mutation,
  stateful CLI session subcommand, incremental cache reuse).

Deferred:

- Branch: forking a session from a prefix into a separate identity
  in the same process.
- Explicit conflict-resolution modes for replay and restore beyond
  prefix-only.
- Rebind-by-name semantics for replay / restore.
- Transcript checkpoints / packfiles / binary packing.
- Transcript schema migration tooling (becomes load-bearing the
  first time `format_version` is bumped).
- In-language (Loko-from-Loko) mutation calls.
- A stateful `interloko_lab` session subcommand.
- `RecomputeStrategy::IncrementalReuse` through
  `IncrementalCompilerSession::apply_mutation`.

## Implemented: Replay Mutation Logs Against A New Base Source

- `SessionArtifacts::replay_against(new_source: &SourceFile) ->
  ReplayOutcome` turns the mutation log from a session-local history
  into a portable transcript. Replay builds an ephemeral session from
  `new_source`, walks the live mutation log in application order,
  applies each entry's request through the existing candidate-and-swap
  discipline using the entry's *original* mutation id, stops at the
  first failure, and returns the applied prefix plus the failure (if
  any) plus the ephemeral artifacts. The original session is byte-for-
  byte unchanged regardless of replay outcome.
- Prefix replay only. Replay does not silently skip failed entries;
  skip-failed would change the meaning of a transcript and is deferred
  to its own explicit named mode if it ever lands.
- Original mutation ids are preserved. Applied entries keep their
  `mutation_id`; the ephemeral session's `MethodOrigin::Session`
  markers match the original log; the ephemeral session's
  `next_mutation_id` is finalized as `max(applied_ids) + 1` so
  subsequent `apply_mutation` calls on adopted artifacts never collide
  with replayed ids.
- The apply path is refactored so replay can supply explicit ids:
  `apply_mutation` becomes a thin wrapper that supplies
  `self.next_mutation_id` and delegates to the new internal
  `apply_session_mutation_with_id(mutation, id)`. Replay loops over
  the log calling that helper with each entry's original id. The
  next-id counter advances via `max(current, id + 1)` so normal apply
  behavior is unchanged and replay never rolls the counter backward.
- `ReplayOutcome` carries `applied: Vec<AppliedMutation>`, `failed:
  Option<ReplayFailure>`, and `artifacts: SessionArtifacts`.
  `ReplayFailure` carries `mutation_id`, `log_index`, the original
  `request`, and a structured `cause: MutationError` (reusing the
  existing apply-time error taxonomy: `UnknownGeneric` when the target
  generic no longer resolves, `GenericNameMismatch` when an id now
  points at a different-named generic, `SemanticFailed` when a
  specializer class is gone, `DuplicateSpecializer` when the new base
  already defines the method, and so on).
- Seven tests pin the contract: replay against the pristine base
  reproduces full log + dispatch; a harmless base edit still replays;
  a base with no classes and no generics fails at mutation 0 with
  `cause = UnknownGeneric`; a base where `area` has been reordered
  behind a fresh `score` generic surfaces
  `cause = GenericNameMismatch { expected: "score", actual: "area" }`
  rather than silently applying onto the wrong generic; a base
  without a specializer class fails at the relevant later mutation
  with `cause = SemanticFailed` carrying a `DiagnosticCode::UnresolvedClass`
  (and the earlier mutation has already applied to the ephemeral
  session); a base that already defines the method a replayed mutation
  would add fails with `cause = DuplicateSpecializer` at the
  colliding entry; replay preserves ids across a log with gaps and
  finalizes `next_mutation_id` to `max(applied) + 1` so a subsequent
  apply on adopted artifacts gets the right id. The success-path
  tests and the failure-path tests both assert the original session
  is byte-for-byte unchanged.
- The 172 pre-existing apply/rewind tests pass against the refactored
  apply path unchanged.
- Replay has no `interloko_lab` surface in this slice for the same
  reason rewind doesn't: each lab invocation is stateless.
- See [`docs/eop_mutation.md`](docs/eop_mutation.md) for the replay
  contract, the prefix-only framing, the id-preservation rule, and
  the still-deferred follow-ups (explicit conflict-resolution modes,
  rebind-by-name replay, branch, log persistence, in-language
  mutation, stateful CLI session subcommand, incremental cache reuse).

Deferred:

- Explicit conflict-resolution modes for replay beyond prefix-only
  (skip-failed, strict-prefix, drop-after-id, etc.).
- Rebind-by-name replay for cases where the new base reordered
  generics or classes but kept the names.
- Branch: forking a session from a prefix of the log into a separate
  identity.
- Log persistence / on-disk serialization (replay makes this much
  easier; it stays a separate slice).
- In-language (Loko-from-Loko) mutation calls.
- A stateful `interloko_lab` session subcommand.
- `RecomputeStrategy::IncrementalReuse` through
  `IncrementalCompilerSession::apply_mutation`.

## Implemented: Rewind The In-Memory Mutation Log

- `SessionArtifacts::rewind_to(target: Option<u32>) ->
  Result<RewindOutcome, RewindError>` truncates the live mutation log
  at `target` (or back to pristine when `None`) and rebuilds session
  state through the existing candidate-and-swap discipline. Mutation
  ids are stable session event ids: a private monotonic counter on
  `SessionArtifacts` advances on every successful `apply_mutation`
  and is preserved across every `rewind_to`. Retained log entries
  keep their `mutation_id`; dropped ids are permanently retired. The
  next `apply_mutation` after a partial rewind gets the next
  never-used id (not `retained_count`); the live log can carry a
  deliberate gap where a dropped id used to be. Rewinding to a
  dropped id surfaces `RewindError::TargetOutOfRange`.
- Rewind is an operation *on* the log, not an entry *in* it. The
  truncated tail is reported in `RewindOutcome::dropped` but is never
  appended to the log. The mutation log stays a forward transcript of
  domain mutations; replaying it against the pristine source still
  reproduces current state.
- `SessionArtifacts` carries a private `pristine_source: String` field
  fixed at first compile and preserved byte-for-byte across every
  `apply_mutation` and `rewind_to` call. A new
  `compose_source_from_log(pristine, requests)` helper deterministically
  produces the composed source text from pristine plus any subset of
  log requests; both `apply_add_method` and `rewind_to` use it. The
  helper is also the seed for the future replay-against-changed-base
  slice.
- `RewindOutcome` carries `retained_count`, `dropped`, and
  `recompute_strategy: RecomputeStrategy` (always
  `FullCandidateRebuild` in this slice). `RewindError` carries
  `TargetOutOfRange { target, current_count }` for invalid targets
  and `RuntimeValidationFailed { error }` as a safety net.
- `SessionMutation` and `AppliedMutation` gain `Eq + PartialEq` so
  `RewindOutcome` is comparable and tests can `assert_eq!` on log
  entries.
- Five tests cover the rewind contract: rewind to first id keeps the
  first method and drops the second (with the triangle method's
  dispatch now trapping); rewind to pristine empties the log and
  restores original dispatch; out-of-range target preserves state
  byte-for-byte; next `apply_mutation` after partial rewind assigns
  the expected id; EOP `mutation:m<id>` records shrink in lockstep
  with the log.
- Rewind has no `interloko_lab` surface in this slice. The lab is
  stateless per invocation, so a standalone rewind would have nothing
  to rewind; a stateful session subcommand is deferred along with the
  rest of the long-lived session UX.
- See [`docs/eop_mutation.md`](docs/eop_mutation.md) for the rewind
  contract, the operations-on-log boundary, and the still-deferred
  follow-ups (branch, public replay against changed base, persistence,
  in-language mutation, stateful CLI session subcommand, incremental
  cache reuse).

Deferred:

- Branch: forking a session from a prefix of the log into a separate
  identity.
- Public replay of a log against a changed base source.
- Log persistence / on-disk serialization.
- In-language (Loko-from-Loko) mutation calls.
- A stateful `interloko_lab` session subcommand.
- `RecomputeStrategy::IncrementalReuse` through
  `IncrementalCompilerSession::apply_mutation`.

## Implemented: Explicit In-Memory Mutation Log

- `SessionArtifacts` carries a private `mutations: Vec<AppliedMutation>`
  field exposed through `SessionArtifacts::mutations(&self) ->
  &[AppliedMutation]`. Each `AppliedMutation` records the assigned
  `mutation_id`, the original `SessionMutation` request, and the
  returned `MutationOutcome`.
- The log is the single source of truth for session-mutation provenance.
  `apply_add_method` walks the log on every candidate-and-swap rebuild
  and re-applies `MethodOrigin::Session { sequence: entry.mutation_id }`
  to the method at `entry.outcome.inserted_method`. The
  `saturating_sub`-based pristine-count arithmetic that quietly
  underpinned the previous bug fix is gone; the redundant
  `mutation_sequence: u32` field on `SessionArtifacts` is gone too.
- Each `AppliedMutation` projects as an EOP environment object
  `mutation:m<id>` with kind `Mutation`, origin `Session`, storage
  `TransientInMemory`. Workbench dumps surface the log alongside
  classes, generics, methods, dispatch sites, and diagnostics. The
  storage label and inspection text never use "saved", "stored", or
  "reused" wording.
- `interloko_lab invoke-affordance ... add-method` reports
  `log_size=<N>` on the outcome line. The lab is stateless per
  invocation, so a successful add-method always shows `log_size=1`
  today; the field exists so the log is visible from the CLI and so
  future stateful surfaces report it with the same wording.
- See [`docs/eop_mutation.md`](docs/eop_mutation.md) for the log shape,
  the in-memory fence, and the deferred follow-ups (rewind, branch,
  replay against a changed base, persistence, in-language mutation,
  stateful CLI session surfaces, incremental cache reuse, and session
  restore).

Deferred:

- Log rewind, undo, branch, and replay-against-changed-base.
- Persistence or any on-disk log format.
- In-language (Loko-from-Loko) mutation calls.
- A stateful `mutations <file>` CLI subcommand.
- `RecomputeStrategy::IncrementalReuse` cache reuse through
  `IncrementalCompilerSession::apply_mutation`.

## Implemented: Live EOP Method Mutation

- `interloko_runtime` carries generic arity on `RuntimeGeneric` and exposes
  `RuntimeLayout::add_method`, which inserts a method into an existing
  generic and rejects unknown generics, unknown specializer classes, and
  specializer vectors whose arity disagrees with the target generic.
- Semantic IR has a `MethodOrigin::Session { sequence }` provenance marker
  so session-inserted methods are distinguishable from source-origin and
  slot-setter methods without losing any dispatch-equivalent behavior.
- `SessionArtifacts::apply_mutation(SessionMutation::AddMethod { generic,
  method_source })` accepts a full Loko `define method ...; end;` clause,
  parses it through the ordinary parser, compiles a candidate session
  state, validates it, and atomically swaps it in. Failures (parse,
  semantic, runtime, duplicate specializer, unknown generic, generic-name
  mismatch, or accidental class/generic/function addition) leave the live
  session untouched.
- `MutationOutcome` reports the mutation id, inserted method id, precise
  `affected_artifacts` (top-level pipeline + the new method's
  `MethodLookup`/`MethodLowering`/`MethodTape` + the affected generic's
  `GenericDispatcher`), and a `RecomputeStrategy`. The only current
  strategy is `FullCandidateRebuild`; CLI and EOP wording must not surface
  cache-reuse claims yet.
- `interloko_lab invoke-affordance <file.loko> <generic:gN> add-method
  --source <method.loko>` executes the affordance and prints a one-line
  outcome summary including the strategy name.
- See [`docs/eop_mutation.md`](docs/eop_mutation.md) for the boundary,
  validation order, provenance, and the deferred mutations
  (`retract-method`, `edit-body`, `define-class`, structural method
  sources, incremental-cache reuse, and session restore).

Deferred:

- True `IncrementalCompilerSession::apply_mutation` cache reuse with a
  new `RecomputeStrategy::IncrementalReuse` variant.
- Mutation affordances other than `add-method`.
- Structural frontend-form method sources (no text input).
- A capability policy for which callers can request which mutations.
- Any persistent-image or session-restore path.

## Implemented: Structural Staged Expressions

- The staged calculator adapter now emits structural frontend expression nodes
  for integers, names, calls, and `+`, `-`, `*` binary expressions.
- `ast::lower_with_frontend_forms` lowers those expression nodes directly into
  AST expression bodies. It no longer reparses generated Loko text for staged
  function bodies.
- Source-data dumps and reports project calculator bodies as records such as
  `frontend-integer`, `frontend-name`, `frontend-call`, and
  `frontend-binary`.
- Materialized Loko remains available on staged regions for dumps, reports,
  and human comparison, but it is only a debug projection.

Deferred:

- A complete structural expression language for every ordinary Loko expression.
- Source-authored grammar definitions, semantic actions, macro hygiene, and a
  capability policy for user-provided grammar extensions.

## Implemented: Direct Frontend Forms To AST

- Staged grammar regions are no longer materialized into the source that feeds
  the ordinary parser. The grammar frontend blanks staged regions while
  preserving byte offsets and line structure, then carries structural frontend
  forms beside the syntax tree.
- `ast::lower_with_frontend_forms` now lowers staged `FrontendForm::Function`
  and `FrontendForm::Class` values directly into AST definitions. Generated
  functions, classes, slots, and expression bodies carry stable IDs and source
  spans tied to the original staged region.
- Materialized Loko remains available on each staged region for dumps, reports,
  and human comparison, but it is an inspectable projection rather than the
  semantic compiler input.
- Grammar extension emissions now report the generated EOP object where
  practical, for example `generated=function:f0` or `generated=class:c0`.
- Staged calculator and records fixtures still compile and run through the
  normal syntax -> AST -> semantic IR -> lowered IR -> execution_tape pipeline.

Deferred:

- Source-authored grammar definitions, semantic actions, macro hygiene, and a
  capability policy for user-provided grammar extensions.

## Implemented: Staged Grammar Extension Records

- Registered staged adapters now expose structured
  `GrammarExtensionDescriptor` records. Descriptors name the adapter, projected
  grammar, activation directive, region delimiters, accepted region shape,
  emitted frontend form kinds, provenance rules, and required capabilities.
- Parse reports now carry concrete extension activation records, emitted
  frontend-form records, and extension provenance records alongside the older
  staged-region summary.
- EOP projection now exposes `grammar-extension:*`,
  `grammar-extension-activation:*`, `grammar-extension-emission:*`, and
  `grammar-extension-provenance:*` objects. These make staged parsing visible
  as protocol data instead of trait-method convention.
- `interloko_lab extensions <file.loko>` and the `show-grammar-extension`
  affordance explain registered extensions, source activation spans, emitted
  forms, capabilities, and charged provenance rules.

Deferred:

- Source-authored grammar extension definitions.
- A real capability policy beyond explicit capability records.
- Semantic actions that emit structural forms without trusted Rust adapters.

## Implemented: Grammar Frontend Registry Cleanup

- Replaced the closed `GrammarEnvironmentSelection` enum with a registry-token
  selection type. Callers now use `GrammarEnvironmentSelection::base_loko()`,
  `GrammarEnvironmentSelection::derived_host()`, or
  `GrammarFrontendRegistry::builtin().environment_selection(token)`.
- Added `GrammarFrontendRegistry`, `GrammarEnvironmentProvider`, and
  `StagedGrammarAdapter` as the explicit protocol for language environment
  records and directive-activated staged grammar adapters.
- The built-in registry now owns `base-loko`, `derived-host`, `calculator`,
  and `records` lookup instead of scattering string matches through the
  frontend and tools.
- `interloko_lab` and `interloko_corpus` validate `// grammar:` headers
  through the registry, so new registered environments do not need duplicated
  tool-side match arms.
- `CompilerSession` and `IncrementalCompilerSession` now own the grammar
  registry they compile with; the built-in registry remains the default, but
  alternate static registries can be passed through the session boundary.
- `interloko_lab registry <file.loko>` plus EOP objects expose the session
  registry, registered language environments, and staged adapters. The same
  data is available through the `grammar-registry:session` object and
  `show-grammar-registry` affordance.
- Staged grammar discovery now resolves `// grammar-activate:` and
  `// staged-loko:` directives through the registry and reports the registered
  adapter set in expectation diagnostics.
- `docs/grammar_mop.md` describes the registry protocol and the public API
  migration away from enum variants.

Deferred:

- Source-authored grammar registration.
- Non-static runtime registry mutation.
- Capability policy for host-provided grammar adapters.

## Implemented: Timing Metrics And Execution Traces

- Session artifacts now carry work counters plus elapsed timing fields for the
  source, grammar/environment, parse, AST, semantic, diagnostic, lowering,
  tape, runtime, method, and generic-dispatch artifacts already reported by
  the incremental engine.
- Stable artifact keys and fingerprints remain independent of timing data.
  Timing is presentation/reporting metadata, not an input to cache identity.
- Foundational compiler builds remain `no_std` by default. Tools opt into the
  compiler `std` feature to collect wall-clock nanoseconds; `no_std` builds
  still expose the same metrics shape with zero wall-clock duration.
- Added structured execution trace capture over the public
  `execution_tape::trace::TraceSink` API. Trace records cover run start/end,
  call-frame enter/exit, host-call enter/exit, and optional instruction
  events.
- `interloko_lab run --trace <file.loko>` prints the normal result followed by
  the trace. `interloko_lab trace <file.loko>` prints only the trace, and
  `--instructions` includes per-instruction events.
- HTML reports now include per-stage duration/work summaries, artifact cards
  with timing/work fields, a compact execution timeline, and a collapsible raw
  trace dump.

Deferred:

- Persistent profiling storage.
- Live trace streaming, watch mode, and browser RPC.
- Parallel scheduling and scheduler-owned timing aggregation.
- Export to external trace/profiling formats.
- Instruction-level timeline visualization by default.

## Implemented: Static HTML Workbench Reports

- Added `interloko_lab report <file.loko> --output <report.html>` as a
  single-command demo path for compiler/session artifacts.
- The command compiles through the existing session pipeline with runtime
  execution requested, then writes one deterministic self-contained HTML file
  with embedded CSS, JavaScript, and structured JSON data.
- The report first viewport shows source file, runtime result, diagnostic
  count, grammar stack, staged-region count, and a compact artifact pipeline.
- Source rendering includes line numbers plus provenance highlights for staged
  grammar activation directives, frontend-form source spans, and diagnostics.
- Staged-region rendering shows the current expansion chain:
  adapter -> frontend forms -> generated AST/semantic/EOP objects, with
  materialized Loko kept as a debug projection.
- The report exposes generated classes, slots, accessors, setter generics,
  methods, functions, object ids, affordances, structured diagnostics, artifact
  keys, dependency graph text, lowered IR, and `execution_tape` disassembly.
- A small embedded script lets readers click related source spans, staged
  regions, frontend forms, objects, and artifacts to highlight connected nodes
  where current artifacts expose those relations.
- Added lab tests covering staged-records report generation and the empty-state
  path for ordinary Loko sources.
- `docs/interloko_report.md` documents the command as the preferred static demo
  path and explains how this can evolve into a live workbench later.

Deferred:

- Live editing, watch mode, server/browser RPC, and incremental browser
  refresh.
- A shared presentation/data API outside `tools/interloko_lab`.
- Rich graph layout beyond compact cards and relation highlighting.
- Richer relation layout and generated-object explanations.

## Implemented: Structural Frontend Forms And Records Grammar

- Added `CalculatorRegionGrammar` and
  `RecordsRegionGrammar` under `StagedLokoEnvironment` as grammar MOP slices
  where grammar records participate in parsing, not only inspection.
- Added structural frontend forms for staged adapter output:
  `frontend-function`, `frontend-class`, `frontend-slot`, and transitional
  raw Loko expression text. Forms carry original staged source spans and are
  projected through inert `source_data`.
- Staged adapters now emit frontend forms first. The grammar frontend preserves
  materialized Loko for inspection, while AST lowering consumes the structural
  forms directly.
- Added an internal staged grammar adapter registry. The registered adapters
  today are `calculator` and `records`.
- A frontend metadata directive selects a staged grammar:

  ```text
  // grammar-activate: calculator
  ```

  This is workbench/frontend metadata, not Loko syntax.
- One or more later comment-delimited regions:

  ```text
  // staged-loko: calculator
  helper = 40
  // end-staged-loko
  ```

  are routed through the calculator adapter and emit frontend function forms
  that materialize to ordinary Loko source:

  ```text
  define function helper()
    40
  end
  ```

- A records region:

  ```text
  // grammar-activate: records

  // staged-loko: records
  record circle
    radius: <integer>
  end
  // end-staged-loko
  ```

  emits a frontend class form that materializes to:

  ```text
  define class <circle> (<object>)
    slot radius :: <integer>, required-init-keyword: radius:;
  end
  ```

- The existing pipeline still runs through the normal AST, semantic IR,
  lowering, and tape layers. `ast.rs` is the only downstream layer that sees
  staged frontend forms; `sem`, `lower`, and `tape` do not perform grammar
  lookup.
- Parse trace now shows grammar activations for the base source file and each
  staged region, plus a `staged regions:` section with adapter name, activation
  directive span, frontend forms, debug materialized source, and inert
  `source_data`.
- MOP/EOP output includes `grammar:calculator`, `grammar:records`, activation
  objects such as `grammar-activation:calculator/r0`, and staged-region objects
  such as `staged-region:records/r0`.
- Added lab and corpus fixtures showing that staged calculator and records
  programs run and remain inspectable.

Deferred:

- Source-authored grammar definitions.
- Public staged grammar registry and capability policy.
- Macro hygiene and name-introduction policy.
- Richer structured expression forms instead of transitional raw Loko text.
- Semantic actions beyond inert structural source-data emission.

## Implemented: Grammar MOP Follow-Up Slices

- Added `DerivedHostEnvironment` and `DerivedHostGrammar` as a trusted
  read-only grammar fixture that extends `BaseLokoGrammar`.
- Grammar records now distinguish direct rules from effective rules. The
  derived fixture overrides `function-definition` and inherits the rest, while
  still delegating CST construction to the base hand-written parser.
- Source files carry an explicit `GrammarEnvironmentSelection`, defaulting to
  `base-loko`. Workbench and corpus files can select `derived-host` with:

  ```text
  // grammar: derived-host
  ```

- Incremental cache reports now treat grammar-environment changes as explicit
  grammar-stack changes. The source text artifact can be reused while grammar
  stack, grammar parse, and downstream grammar-dependent artifacts are marked
  recomputed/affected.
- Parse reports include grammar diagnostic facts that attach syntax
  diagnostics to the active environment, grammar, activation, effective rule,
  source span, diagnostic code, and structured expected fragment.
- `show-parse-trace`, MOP dumps, EOP records, lab fixtures, and corpus cases
  expose the derived grammar stack, effective-rule resolution, and structured
  grammar diagnostics.
- Added `interloko_compiler::source_data`, a small inert structural data model
  for frontend protocol facts. Grammar rules now carry structured rule data in
  addition to their human-readable projected expression.
- `docs/grammar_mop.md` describes derived grammars, grammar headers,
  structured diagnostic facts, and the source-data boundary.

Deferred:

- Source-authored grammar declarations.
- Semantic actions and macros.
- Host execution from grammar actions.
- Capability policy for grammar/action activation.
- Replacing the hand-written parser.

## Implemented: Read-Only Grammar MOP Projection

- Added `interloko_compiler::grammar_frontend` as the frontend boundary around
  the existing hand-written Loko parser. It wraps `syntax::parse_file` and
  returns a `GrammarParseReport` without changing AST, semantic IR, lowering,
  or tape emission consumers.
- Projected `BaseLokoEnvironment` and `BaseLokoGrammar` as read-only grammar
  metaobjects with stable EOP ids:
  - `language-environment:base-loko`;
  - `grammar:base-loko`.
- Added direct compiler-projected grammar-rule records for the current parser
  surface, including source file, class/function/generic/method definitions,
  slot declarations/options, expression layers, vector literals, arguments, and
  keyword arguments.
- Added grammar activation and parse provenance records. Parse trace output
  reports the resolved environment, grammar stack, activation span, root rule,
  and per-rule CST node counts.
- Added explicit query artifacts and dependency edges:

  ```text
  source -> environment-resolution -> grammar-stack -> grammar-parse -> parse
  ```

- MOP and EOP dumps now include language environments, grammars, grammar rules,
  and grammar activations. `interloko_lab inspect` can inspect
  `grammar:base-loko` and `language-environment:base-loko`.
- Added read-only affordances:
  `show-grammar-stack`, `show-grammar-rule`, and `show-parse-trace`.
- Tests cover normal source-to-tape compilation through the existing pipeline,
  lab inspection/invocation of grammar affordances, grammar artifact edges, and
  stable grammar object ids across harmless source edits.
- `docs/grammar_mop.md` documents the implemented boundary and deferred staged
  grammar design.

Deferred:

- Source-authored grammar syntax.
- Macros and semantic actions.
- Actual grammar overriding or derived grammar execution.
- OMeta/Ohm compatibility.
- Arbitrary host execution from grammar actions.
- Parser rewrite.

## Implemented: Interloko Runtime Boundary

- Added `crates/interloko_runtime` as the executable runtime crate for the
  current Interloko-shaped object model.
- Moved runtime host-call symbols and signatures into `interloko_runtime`,
  including object construction, slot get/set, boxing, class tests/casts,
  literal construction, vector helpers, and read-only MOP query calls.
- Moved `RuntimeLayout`, `RuntimeClass`, `RuntimeSlot`, `RuntimeGeneric`,
  `RuntimeMethod`, and `HostObjectKind` into `interloko_runtime`.
- Added a runtime-owned `ValueKind` that covers only executable value
  categories. Compiler-only semantic states such as known callables and errors
  remain in `interloko_compiler::sem::ValueKind`.
- Moved `InterlokoHost`, native object storage, boxed `<object>` storage, string,
  symbol, vector, metaobject, and host-backed adapter support into
  `interloko_runtime`.
- `interloko_compiler::runtime` owns only `runtime_layout_from_sem`, the
  sem-to-runtime layout adapter; executable runtime APIs are imported from
  `interloko_runtime` directly.
- Added runtime-owned object protocol helpers for class/generic/method lookup,
  superclass chains, effective slots, and slot capability reporting.
- Added `RuntimeError` for structured runtime object-operation failures, with
  `execution_tape::host::HostError` mapping only at the tape host boundary.
- Host-backed adapters now report `RuntimeError`, making host-backed slot reads
  part of the same runtime slot protocol as native slot reads.
- Lowering builds runtime layout values through the compiler adapter, while
  tape emission imports host symbols/signatures directly from
  `interloko_runtime`.
- Host-backed examples now distinguish semantic declaration kinds from runtime
  adapter value kinds.

Deferred:

- Persistent runtime/image state.
- Runtime scheduler, async/effect handler stack, or dispatch caches.
- Moving semantic IR, lowering, sessions, diagnostics, or tape emission out of
  `interloko_compiler`.
- Further crate extraction beyond the current Interloko compiler/runtime split.

## Implemented: Interloko Model Boundary

- `docs/interloko_boundary.md` records the naming split:
  - Interloko is the core substrate for programmable object spaces;
  - Vivarium is the later collaborative environment/product layer;
  - Loko is the current source notation and `.loko` file format.
- Added `crates/interloko_model` as the first `no_std` substrate crate.
- Moved durable shared model identifiers into `interloko_model`:
  `ClassId`, `SlotId`, `GenericFunctionId`, and `MethodId`.
- Moved `BuiltinValueClass` into `interloko_model`, including deterministic
  declaration order, source name lookup, and class name reporting.
- `interloko_compiler::sem` re-exports the moved types so existing compiler,
  runtime, examples, and tools can keep their current semantic paths during
  the extraction.
- `ValueKind` remains in `interloko_compiler::sem` because it still contains
  compiler-only callable and error states.
- Runtime layout records and `InterlokoHost` remain in `interloko_compiler` until a
  later `interloko_runtime` extraction can avoid pulling semantic IR ownership
  across the crate boundary.
- Crate/package names remain unpublished and provisional.

Deferred:

- Full repo rename.
- `vivarium_*` crates or collaboration/presence/permission features.
- Language rename to Loko.
- Moving runtime host execution out of `interloko_compiler`.
- Moving semantic IR, lowering, sessions, diagnostics, or tape emission.
- Publishing any Interloko crate.

## Implemented: Named Read-Only MOP Protocol

- Loko code now uses canonical read-only MOP protocol names:
  `object-class`, `class-direct-superclass`, `class-direct-slots`,
  `class-effective-slots`, `slot-name`, `slot-type`,
  `slot-init-keyword`, `slot-mutable?`, `slot-getter`, `slot-setter`,
  `generic-function-name`, `generic-function-methods`,
  `method-generic-function`, and `method-specializers`.
- The protocol has one visible vocabulary. The older first-slice names such as
  `class-superclass`, `class-slots`, `generic-name`, `generic-methods`, and
  `method-generic` are not accepted as aliases.
- `object-class` returns a class metaobject through the ordinary boxed
  `<object>` path, so code can ask for the class of native objects, primitive
  integers, and metaobjects.
- Class slot queries now distinguish direct source slots from effective
  finalized slots. Effective slots remain the runtime-layout descriptors used
  by construction, reads, writes, and inherited slot inspection.
- Slot descriptors expose source name, declared type text, init keyword,
  mutability, getter name, and generated setter generic when one exists.
- Runtime and tape still implement all protocol operations through ordinary host
  calls such as `runtime.class-effective-slots` and
  `runtime.generic-function-methods`; there are no MOP-specific tape opcodes.
- New corpus programs under `corpus/pass/mop` exercise object-class, direct vs
  effective slot lists, slot descriptor facts, setter protocol lookup, and
  method-to-generic lookup.
- `docs/mop_protocol.md` describes how to use the read-only protocol and why
  `slot-type` and `slot-getter` are intentionally conservative in this slice.

Deferred:

- First-class type metaobjects for `slot-type`.
- Getter accessor metaobjects or generic getter protocols.
- Runtime mutation of classes, slots, generics, or methods.
- Method combination, `next-method`, and user-extensible dispatch protocols.
- Persistent image mutation, capability policy, and optimizer assumptions over
  reflective protocol calls.

## Implemented: First-Class Read-Only MOP Metaobjects

- Class references such as `<circle>` now lower as runtime class metaobjects
  when used as ordinary values. Existing optimized paths still keep class ids
  internally for `make`, dispatch tests, casts, and slot/runtime layout work.
- Bare generic-function references used as values now produce generic-function
  metaobjects instead of remaining only compile-time callable tokens. `apply`
  still recognizes known generic metaobjects and emits ordinary generic calls.
- The runtime layout now carries generic-function and method metadata alongside
  class layout, so runtime host calls can answer basic MOP queries from the
  same facts used by dumps and dispatch.
- Loko code can call read-only MOP protocol operations:
  - `class-name`;
  - `class-direct-superclass`;
  - `class-effective-slots`;
  - `generic-function-name`;
  - `generic-function-methods`;
  - `method-generic-function`;
  - `method-specializers`.
- Minimal immutable vector inspection is available through `vector-size` and
  `vector-ref`, which lets Loko code inspect method and slot vectors returned
  by the MOP.
- Generated setter generics and setter methods are visible through the same
  generic/method metaobject path as source-authored definitions.
- Host-backed classes participate in the class metaobject path, but this is
  explicit Interloko metadata, not automatic Rust reflection.
- Lowered IR emits ordinary runtime host calls such as `runtime.class-name`,
  `runtime.generic-function-methods`, and `runtime.method-specializers`. Tape
  still has no MOP-specific opcodes.
- Semantic, lowered, MOP/EOP, and corpus dumps now agree that classes,
  generics, methods, and slot descriptors are object-system metaobjects.

Deferred:

- Runtime mutation of classes, generics, methods, or slots.
- User-defined metaclasses and a public AMOP-compatible protocol.
- Method combination, `next-method`, and extensible dispatch protocols.
- Rich slot-descriptor mutation protocols beyond exposing descriptor objects in
  `class-effective-slots`.
- Persistent image identity, permissions, transactions, and automatic host/Rust
  reflection.

## Implemented: Multi-Argument Generic Dispatch

- Generic dispatch now considers the full required argument vector instead of
  only a single dispatch parameter.
- Source methods retain all specialized parameters in AST and semantic IR.
- Semantic methods now carry specializer vectors. `None` in that vector
  represents source `<object>`, because `<object>` is still Interloko's top value
  category rather than a nominal class id.
- A method applies when every specialized parameter accepts the runtime
  argument class.
- Specificity is a partial order: a method is more specific only when it is at
  least as specific in every position and strictly more specific in at least one
  position.
- The semantic pass diagnoses provably ambiguous overlapping method sets before
  lowering.
- Lowered generic dispatchers test all specialized arguments and call methods
  with ordinary casts/calls/branches. Tape still has no generic-specific opcode.
- Session, MOP, EOP, and dispatch-site dumps expose specializer vectors,
  dispatch parameter vectors, and tuple-shaped applicability.
- Setter generics keep their `(new-value, object)` call shape and generated
  setter methods participate in vector dispatch.

Deferred:

- `next-method` / `call-next-method`.
- Method combination.
- Keyword/rest parameter dispatch.
- Singleton, union, limited, or sealed-domain dispatch.
- User-extensible dispatch protocols and dispatch caches.

## Implemented: Writable Native Slot Setters

- Surface syntax now accepts `object.slot := value`.
- Assignment is semantic sugar for the compiler-synthesized setter generic named
  `<slot>-setter`, called as `(new-value, object)`.
- Mutable native slots synthesize:
  - a visible setter generic;
  - a setter method specialized on the slot owner class;
  - semantic `SlotWrite` in the generated method body.
- Setter calls return the assigned value, so assignment is an expression.
- Explicit setter-call spelling such as `radius-setter(10, circle)` works for
  synthesized mutable native slots.
- Constant slots, unknown slots, non-object receivers, host-backed slots, and
  wrong assigned value types produce structured diagnostics.
- Lowered IR now has typed `slot.write` instructions, and tape emission lowers
  them to ordinary runtime host calls (`interloko.slot_set`), not special bytecode.
- Runtime native object storage mutates the selected slot and returns the stored
  value.
- MOP/EOP/session dumps expose generated setter generics and methods as
  compiler-generated metaobjects with `slot-setter(cX.sY)` origin labels.

Deferred:

- Generalized Dylan setter places such as `radius(obj) := value`.
- User-defined setter methods and method combination.
- Writable host-backed slots.
- Transactions, persistence, slot deletion/unbinding, and optimizer removal of
  setter generic overhead.

## Implemented: Milestone 0

- Workspace scaffold with a `no_std + alloc` compiler crate and a separate runnable example crate.
- Tiny Dylan-like parser for:
  - `define function ... end;`
  - parameters
  - `let` bindings
  - integer literals
  - `+`, `-`, and `*`
  - direct function calls
- Lossless-enough syntax layer:
  - original source retained
  - all tokens retained, including whitespace and comments
  - CST nodes carry source spans
- AST lowering from CST.
- Semantic lowering with:
  - `FunctionId`
  - `LocalId`
  - direct-call resolution
  - unresolved-name and arity diagnostics
- `execution_tape` emission for direct calls and `i64` arithmetic.
- End-to-end driver that compiles and runs zero-argument `main`.
- Example program in `examples/forty_two` that returns and prints `42`.

## Implemented: Generic Dispatch Slice

- Class definitions with nominal `ClassId` values and slot declarations with `SlotId` values.
- Generic function declarations with `GenericFunctionId` values.
- Method definitions with `MethodId` values and one nominal class specializer.
- AST and semantic support for:
  - `make(<class>, slot: value)` object construction
  - `receiver.slot` reads
  - `GenericCall` for generic calls
  - `DirectCall` for ordinary function calls
- Runtime object host support for:
  - object construction
  - runtime class lookup
  - integer slot reads
- `execution_tape` emission using only ordinary calls, branches, and host calls.
- Generated dispatcher functions for generic calls; no generic-specific tape opcode.
- Thin invalidation shell using `invalidation 0.2` for source, parse, AST, bind, method lookup, method lowering, method tape, generic dispatcher, and program artifacts.
- Targeted invalidation tests showing that changing one method affects its lookup/lowering/tape artifacts without affecting unrelated method artifacts.
- Example program in `examples/generic_area` that compiles and runs `area(make(<circle>, radius: 10))` and prints `100`.

## Implemented: Lowered Executable IR and Dumps

- Backend-neutral lowered IR in `interloko_compiler::lower`.
- Lowered function IDs, registers, labels, parameters, local-to-register bindings, and typed values.
- Lowered instructions for:
  - integer constants and runtime metadata constants
  - direct calls
  - generated generic dispatch control flow
  - object construction
  - slot reads
  - integer arithmetic
  - runtime class lookup, equality, branches, returns, and traps
- Generic function calls lower to ordinary calls into generated dispatcher functions.
- Dispatcher functions lower to ordinary class lookup, class-id comparison, branch, method call, return, and trap instructions.
- `execution_tape` emission now consumes lowered IR rather than walking semantic expressions directly.
- Object construction and slot reads are still encoded as ordinary `execution_tape` host calls.
- `CompiledModule::dump_lowered()` returns the lowered IR dump.
- `CompiledModule::dump_tape()` returns the `execution_tape` disassembly.
- Public API migration: callers that previously invoked `tape::emit(&sem)` should now call `lower::lower(&sem)` and then `tape::emit(&lowered)`.

## Implemented: Stable-ish IDs and Artifact Keys

- `SourceFileId` identifies logical source files, with `syntax::parse_file` for callers that have stable paths.
- CST nodes carry `SyntaxNodeId` values.
- AST nodes and definitions carry `AstNodeId` and `DefinitionId` values.
- Semantic definitions carry stable `DefinitionId` values alongside dense semantic IDs.
- Incremental query artifacts expose stable `ArtifactKey` values based on source file, artifact kind, and definition identity.
- `QueryInvalidation` maps stable artifact keys to dense `invalidation 0.2` keys internally.
- Tests cover syntax/AST definition ID preservation across whitespace/comment edits and method artifact key preservation across insertion-driven method renumbering.
- Stability guarantees and non-guarantees are documented in `crates/interloko_compiler/ADR-stable-ids.md`.

## Implemented: Compiler Session and Workbench CLI

- `interloko_compiler::session` provides a no-std-friendly compiler session API over named source files.
- Session artifacts retain:
  - source file identity and text metadata
  - syntax tree
  - AST
  - semantic IR
  - lowered IR
  - tape module
  - diagnostics
  - optional runtime result
- Session dumps cover:
  - CST/syntax
  - AST
  - semantic IR
  - lowered IR
  - `execution_tape` disassembly
  - stable artifact graph with dependencies
- Session diff reports preserved definitions, added/removed/modified definitions, and affected query artifacts.
- `tools/interloko_lab` provides:
  - `run <file>`
  - `dump <file> --stage syntax|ast|sem|mop|eop|lowered|tape|all`
  - `artifacts <file>`
  - `diff <before> <after>`
- CLI fixtures cover the generic-area program, harmless trivia edits, and a focused method body edit.

## Implemented: Small-Program Corpus Runner

- `corpus/pass` contains runnable small programs for:
  - arithmetic and precedence;
  - local bindings;
  - direct function calls;
  - class definitions, object construction, slot declarations, and slot reads;
  - single-method and multi-class generic dispatch.
- `corpus/fail` contains diagnostic examples for:
  - unresolved locals;
  - unresolved functions/generics;
  - arity mismatches;
  - missing slot initializers;
  - unknown slot initializers.
- Each `.loko` corpus file declares expectations in leading comments:
  - `// expect: run N`;
  - `// expect: diagnostics N`;
  - `// expect: diagnostic contains TEXT`;
  - `// expect: dump STAGE contains TEXT`.
- Dump expectations are intentionally substring-based so they act like small
  semantic landmarks rather than full brittle snapshots.
- `tools/interloko_corpus` provides:
  - `check [corpus-root]` to compile every corpus program through
    `interloko_compiler::session`;
  - `list [corpus-root]` to inspect discovered cases;
  - `dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|all` for
    snapshot-like stage output without committing generated dumps.
- Add a new case by placing a `.loko` file under `corpus/pass` or
  `corpus/fail`, adding at least one `// expect:` header line, and running
  `cargo run -p interloko_corpus`.

## Implemented: In-Memory Incremental Compiler Engine

- `interloko_compiler::session::IncrementalCompilerSession` provides an in-memory
  edit/compile API over named sources with stable `SourceFileId` values.
- The incremental session retains the latest `SessionArtifacts` snapshot and a
  cache entry for every stable `ArtifactKey` in the current artifact graph.
- Cache reports cover:
  - parse;
  - AST;
  - semantic binding;
  - diagnostics;
  - method lookup;
  - method lowering;
  - method tape;
  - generic dispatcher;
  - lowered module;
  - tape program;
  - runtime result.
- Each artifact report includes:
  - stable artifact key;
  - `Reused` or `Recomputed` action;
  - invalidated flag;
  - affected flag from semantic edit analysis;
  - deterministic `work_units` and `elapsed_ticks` counters.
- The core compiler crate remains `no_std + alloc`. Timing is represented as
  logical ticks in the core cache report; wall-clock collection is deferred to a
  later std scheduler or workbench wrapper.
- `interloko_lab diff <before> <after>` now runs through the incremental session
  and prints cache status, counters, dependency edges, and the semantic diff.
- `interloko_corpus check` now compiles each program through
  `IncrementalCompilerSession`.
- Tests cover:
  - initial cache population;
  - repeat compile reusing all artifacts;
  - harmless trivia edits reusing most artifacts;
  - focused method-body edits recomputing the edited method's lowered/tape
    artifacts while reusing unrelated method artifacts;
  - definition-changing edits marking broad dependents affected.

### Migration Notes

- `ArtifactKind` and `QueryArtifact` gained public variants for diagnostics,
  whole lowered-module artifacts, and runtime-result artifacts.
- `interloko_lab diff` output is now an incremental cache report followed by the
  existing preserved/changed/affected definition diff.

## Implemented: Control Flow, Booleans, and Comparisons

- Syntax now recognizes boolean literals `#t` and `#f`, integer comparison
  operators `=`, `<`, `<=`, `>`, `>=`, and expression-level
  `if ... then ... else ... end`.
- AST now has explicit boolean, comparison, and conditional expression nodes.
- Semantic IR now has a `Bool` value category alongside `I64`, nominal objects,
  and errors.
- Integer comparisons require `I64` operands and produce `Bool`.
- `if` conditions must be `Bool`, and both branches must produce the same
  semantic value category.
- Diagnostics cover non-boolean `if` conditions, mismatched branch result
  kinds, non-`I64` arithmetic operands, and non-`I64` comparison operands.
- Lowered IR now includes:
  - boolean constants;
  - signed integer comparisons;
  - conditional branches;
  - unconditional jumps;
  - register moves used as explicit join values.
- `execution_tape` emission maps those lowered instructions to ordinary tape
  operations. There are no special Interloko control-flow opcodes.
- Session dumps show control flow in CST, AST, semantic IR, lowered IR, and tape
  disassembly.
- The corpus now includes small programs for boolean conditionals,
  comparisons, `max`, `abs`, nested conditionals, generic dispatch guarded by a
  conditional, and expected control-flow diagnostics.
- Incremental tests cover edits inside one conditional branch and verify that
  the edited method's lowered/tape artifacts recompute while unrelated method
  artifacts are reused.

### Deferred Control-Flow Design

- Loops are deferred until lowered IR has an explicit block model that can
  represent backedges and loop-carried values cleanly.
- Recursion-specific analysis is deferred. Direct calls may use simple
  source-order result categories, but this is not a full fixed-point return-kind
  inference system.
- Short-circuit boolean operators are deferred so they can be specified as
  control-flow forms rather than eager arithmetic operators.

## Implemented: Recoverable Condition Restart Slice

- Condition classes are ordinary nominal classes. There is no separate
  condition declaration form in this slice.
- `signal(condition)` is recognized as a special call-shaped expression where
  `condition` must be a statically known object.
- `handle BODY with restart <condition-class> HANDLER end` establishes one
  lexical restart for `BODY`.
- `with restart e :: <condition-class> HANDLER` additionally binds the signaled
  condition object as a handler-local name, so handlers can inspect ordinary
  slots such as `e.fallback`.
- A `signal` must appear inside an active lexical handler, and the signaled
  condition class must match the nearest active restart's condition class.
- `signal` resumes with the active restart's result category.
- The handled body and restart handler must produce the same semantic value
  category.
- Semantic IR now has explicit `Signal`, `Handle`, and `Restart` nodes.
- Lowered IR uses an internal lexical restart frame in `BodyLowering`:
  - normal body completion moves into the handle result register;
  - `signal` constructs/evaluates the condition object, moves it into the
    optional handler-local register, and jumps to the restart handler label;
  - the handler computes the replacement value and jumps to the shared join.
- `execution_tape` still receives only ordinary object construction, branches,
  jumps, moves, and arithmetic. There are no condition-specific tape opcodes.
- Session dumps and fingerprints include signal/handle/restart structure, so
  handler body edits affect the expected method-level artifacts.
- The corpus includes:
  - invalid-radius area recovery;
  - checked arithmetic fallback;
  - uncaught signal diagnostics;
  - body/restart result-kind mismatch diagnostics;
  - restart class mismatch diagnostics.
- Incremental tests cover edits to a restart handler in one method and verify
  that the edited method's lowered/tape artifacts recompute while an unrelated
  method's artifacts are reused.
- Restart condition bindings are handler-scoped; using the bound name after the
  handler expression is a normal unresolved-local diagnostic.

### Deferred Condition Design

- Cross-call condition signaling and dynamic handler lookup are deferred.
- Runtime handler stacks and dynamic extent edge cases are deferred.
- Multiple restart parameters and condition payload typing beyond ordinary
  nominal slots are deferred.
- Full effect typing, effect inference, restart selection, stack inspection,
  async interaction, and multiple values are deferred.

### Migration Notes

- Public AST and semantic expression enums gained `Signal` and `Handle`
  variants.
- Semantic IR gained the public `Restart` type.
- `ast::Restart` now records an optional condition binding name, and
  `sem::Restart` records the optional bound condition local.

## Implemented: Richer Runtime Slot Type Model

- Slot declarations may now include an explicit type:
  - `slot radius :: <integer>;`
  - `slot valid? :: <boolean>;`
  - `slot cause :: <some-class>;`
- Untyped `slot name;` declarations remain accepted. They now default to
  Dylan-shaped `<object>` in the current slot-initialization model below.
- The implemented slot value categories are intentionally small:
  - primitive signed integer;
  - primitive boolean;
  - nominal Interloko runtime object with an exact class id.
- Object construction checks each keyword initializer against the declared slot
  kind. Mismatches produce diagnostics such as `slot valid? expects <boolean>
  but initializer has <integer>`.
- Slot reads now return the declared slot kind in semantic IR. `e.fallback`
  returns `<integer>` because the `fallback` slot says so, while `flag.valid?`
  returns `<boolean>` and object-reference slots return the referenced nominal
  object kind.
- Lowered IR still uses runtime object construction and `slot.read`
  instructions, but `slot.read` now records and dumps the returned value kind.
- `execution_tape` emission still uses host calls, not native aggregate opcodes:
  - `interloko.make_object` has per-class host signatures with mixed slot argument
    types;
  - `interloko.slot_get` has per-slot host signatures whose return type matches the
    declared slot kind;
  - `interloko.class_of` remains the runtime support used by generic dispatch.
- The runtime object heap now stores slot payloads as `execution_tape::Value`
  values, so integers, booleans, and Interloko object handles round-trip through the
  host boundary.
- AST, semantic, lowered, and tape dumps expose enough of the model to see
  typed slots and typed slot reads in the workbench.
- Corpus coverage now includes mixed typed slots, object-reference slots, typed
  condition payloads, initializer type mismatches, and unresolved slot types.
- Incremental tests cover slot type edits and verify dependent method/lowered/
  tape artifacts recompute.

### Runtime Object ABI Notes

- Interloko objects are still runtime-managed host objects identified by
  `INTERLOKO_OBJECT_TYPE`.
- Object construction passes a class id followed by slot payload registers in
  semantic slot order.
- Slot reads pass an object handle and slot id to the host and receive a value
  whose tape type is determined by the slot declaration.
- This ABI is deliberately compatible with Rust/Interloko interop and dynamic
  language features, but it is not a tape-native struct representation.

### Deferred Native-Aggregate Design

- Tape-native structs, sealed/value classes, aggregate layout selection, and
  object representation optimization remain deferred.
- There is still no inheritance-aware slot overriding, parametric slot types,
  union/intersection types, nullable/object optionality, or full type inference.
- Generic function declarations still do not carry explicit return types; this
  slice only makes slot reads and object construction typed.

## Implemented: Dylan-Shaped Slot Initialization

- Untyped slot declarations now default to `<object>`:
  - `slot value;`
  - `slot value :: <object>;`
- Semantic IR distinguishes the top `<object>` value category from nominal
  object handles:
  - `ValueKind::Any` means any first-slice Interloko value;
  - `ValueKind::Object(ClassId)` means a runtime-managed object with a known
    nominal class.
- Existing integer-like examples and corpus programs now spell integer slots
  explicitly, for example `slot radius :: <integer>;`.
- Slot declarations support the first narrow Dylan-style construction keyword
  options:
  - `init-keyword: external-name:`
  - `required-init-keyword: external-name:`
- A slot's construction keyword defaults to `slot-name:`. Renaming the init
  keyword changes object construction, not the slot read name.
- Object construction now diagnoses:
  - missing ordinary slot initializers;
  - missing required init keywords;
  - unknown construction keywords;
  - duplicate construction keywords;
  - initializer type mismatches.
- AST and semantic dumps show declared type, effective init keyword,
  requiredness, and runtime representation. Semantic fingerprints include slot
  type, keyword, and requiredness so the session diff can report metadata
  changes.
- Lowering uses a boxed top-value representation for `<object>` slots rather
  than treating them as nominal object handles:
  - concrete integers, booleans, and nominal object handles are boxed with
    `interloko.box_value`;
  - boxed top values use the host object type `INTERLOKO_VALUE_TYPE`;
  - nominal instances still use `INTERLOKO_OBJECT_TYPE`;
  - `interloko.make_object` and `interloko.slot_get` use boxed-any signatures for
    `<object>` slots.
- Incremental coverage now distinguishes two slot metadata cases:
  - slot type edits affect runtime layout and recompute lowered/tape artifacts;
  - init-keyword-only edits change bind-level metadata but can reuse lowered IR
    and tape when the runtime layout is unchanged.
- Corpus coverage includes untyped `<object>` slots, required init keywords,
  renamed init keywords, duplicate keyword diagnostics, missing required keyword
  diagnostics, and the existing primitive/nominal typed slot cases.

### Deferred Slot Initialization Design

- There is no unboxing operation for top `<object>` values yet, so a read from an
  untyped slot has kind `<object>` and cannot be used as an integer without
  future type tests, casts, or flow typing.
- `init-function:`, setters, allocation variants beyond instance slots,
  inherited slot specs, and the full Dylan initialization protocol remain
  deferred. `init-value:` and `constant slot` metadata are now implemented in
  the richer slot slice below.
- The boxed-any representation is a first runtime ABI, not a claim about future
  object identity, primitive boxing policy, or optimized layout.

## Implemented: Slot Accessors As Calls

- Each semantic slot now defines a getter accessor whose identity is the slot's
  stable definition ID. This keeps accessor identity stable across harmless
  edits and ties accessor metadata to the slot declaration that created it.
- Surface `receiver.slot` now lowers to semantic `AccessorCall` IR. The semantic
  layer no longer treats slot access as a primitive slot-read expression.
- The explicit accessor spelling `slot-name(receiver)` is supported when no
  direct function or generic with that name takes precedence. `receiver.slot`
  and `slot-name(receiver)` resolve through the same accessor metadata.
- Lowering recognizes known semantic accessor calls and still emits the existing
  lowered `slot.read` instruction. The tape/runtime path is unchanged: this is a
  semantic cleanup plus an inspectable lowering boundary, not a new bytecode
  feature.
- Semantic dumps show `accessor-call` and accessor IDs; lowered dumps continue
  to show optimized `slot.read`.
- Corpus coverage includes both `c.radius` and `radius(c)`, and the incremental
  slot metadata tests now fingerprint accessor metadata through the slot.

### Deferred Accessor Design

- Setters, mutation, inherited accessors, generic function objects, reflective
  accessor enumeration, and optimizer choices beyond direct slot-read lowering
  remain deferred.

## Implemented: Class Names As Class Objects

- Source class names such as `<circle>` now lower to semantic class-object
  values instead of being only raw syntax consumed by `make`.
- Object construction consumes a class-object expression and keeps the known
  class ID for validation and runtime layout. This supports local and direct
  call flow such as `let c = <circle>; make(c, radius: 10)`.
- `class-of(x)` is a semantic expression returning the known class object for a
  known runtime object. Lowering emits the existing class-id operation, and
  generic dispatch continues to use runtime class IDs internally.
- Lowered IR represents known class objects as class IDs with a distinct
  `class(<name>)` value kind. Dumps show semantic `class-object` /
  `class-of` expressions and lowered `const.class` / `class_of` representation.
- Corpus coverage includes direct class-object construction, class objects
  returned through direct calls, and `class-of` feeding `make`.
- Incremental coverage includes a class-reference edit that recomputes the
  dependent lowered module and tape program while reusing unrelated method
  lookup/lowering/tape artifacts.

### Deferred Class Object Design

- Metaclasses, user-defined classes of classes, dynamic unknown class objects,
  reflective class/slot enumeration, class-object mutation, and a full Dylan
  class hierarchy remain deferred.

## Implemented: Typed Direct Function Surface

- Direct function parameters may now carry explicit value-kind annotations:
  - `define function keep?(flag :: <boolean>)`
  - `define function pass(c :: <circle>)`
  - `define function build(klass :: class <circle>)`
- Direct functions may now carry explicit result annotations:
  - `define function keep?() => <boolean>`
  - `define function build() => <circle>`
  - `define function circle-class() => class <circle>`
- Unannotated direct function parameters still default to `<integer>`, keeping
  the existing arithmetic examples source-compatible.
- Unannotated direct function results still use the existing inferred result
  category. Explicit result annotations are checked against the lowered body
  result.
- The first direct-function annotation model supports:
  - `<integer>`;
  - `<boolean>`;
  - `<object>` as boxed top value;
  - exact nominal runtime objects such as `<circle>`;
  - exact class-object values spelled `class <circle>`.
- Semantic declaration pre-pass now records direct function parameter kinds and
  explicit result kinds before lowering function bodies, so direct calls can be
  checked against known signatures.
- Direct function calls are checked at each call site. Mismatches produce
  diagnostics such as `function keep? argument 1 expects <boolean> but got
  <integer>`.
- Lowered IR function signatures now consume the checked semantic kinds.
  Concrete values flowing into `<object>` parameters or returns are boxed with
  the existing `box.any` / `interloko.box_value` runtime path.
- AST, semantic, lowered, and corpus dumps expose annotated parameter and
  result meaning. Stable semantic fingerprints include parameter and explicit
  result categories so signature edits participate in session diffs.
- Incremental coverage includes a signature-only edit from an untyped direct
  function to an `<object>` boundary. The edit recomputes the changed
  whole-module lowering and tape artifacts while reusing unrelated generic
  method lowering/tape artifacts.
- Corpus coverage includes boolean parameters/returns, nominal object
  parameters/returns, class-object returns flowing into `make`, `<object>`
  function boundaries, and argument/return mismatch diagnostics.

### Deferred Direct Function Type Design

- Generic function signature annotations, typed method return annotations,
  optional/rest/keyword parameters, overloads, union types, parametric types,
  full local type inference, dynamic unknown class objects, and dynamic casts or
  unboxing from `<object>` remain deferred.
- Direct function type checking is intentionally exact and local. There is no
  subtyping beyond accepting concrete first-slice values at `<object>` boxing
  boundaries.
- Runtime does not own type checking. It receives already checked lowered IR and
  performs only the existing object/boxed-value host operations.

## Implemented: Typed Generic Contracts

- Generic declarations may now carry explicit parameter and result contracts:
  - `define generic area(x :: <object>) => <integer>;`
  - `define generic keep(x :: <object>) => <object>;`
- Method definitions may now carry explicit result annotations:
  - `define method area(c :: <circle>) => <integer>`
  - `define method keep(c :: <circle>) => <circle>`
- Unannotated generic declarations keep the existing generic-dispatch behavior.
  Their result category remains the first-slice legacy `<integer>` dispatcher
  result.
- The implemented generic parameter contract model is deliberately narrow:
  explicit contracts must be `<object>` or an exact nominal class, because the
  current dispatcher still dispatches over one runtime object argument.
- Method specializers are checked against explicit generic parameter
  contracts. With no inheritance model yet, a nominal generic parameter
  contract accepts only the exact same class; `<object>` accepts any known
  nominal object method specializer.
- Generic call sites are checked against explicit generic parameter contracts.
  A contracted generic call requires a known nominal object argument; boxed top
  `<object>` values must be refined or cast in a later slice before dispatch.
- Method bodies are checked against explicit method result annotations, and
  method results are checked against explicit generic result contracts.
- Lowered dispatcher signatures now use the declared generic result category.
  When a generic returns `<object>` and a method returns a more precise nominal
  object, the dispatcher boxes through the existing `box.any` /
  `interloko.box_value` runtime path.
- AST, semantic, lowered, and corpus dumps expose generic and method contracts.
  Definition and dispatcher fingerprints include contract kinds so signature
  edits participate in session diffs and cache reports.
- Corpus coverage includes typed generic area dispatch, typed generic
  `<object>` results, bad generic call arguments, bad method result contracts,
  and bad method specializers.

### Deferred Generic Contract Design

- Generic dispatch is still one-argument nominal dispatch. Multi-argument
  dispatch, primitive specializers, sealed dispatch optimization, method
  combination, overload resolution, and full generic function type objects
  remain deferred.
- `<object>` generic parameters mean "known runtime object accepted by this
  dispatcher" in this slice, not "any boxed first-slice value". Boxed top values
  require explicit refinement/cast support before they can feed dispatch.

## Implemented: Object Tests, Casts, and Narrow Refinement

- `instance?(value, <class>)` is now recognized as a runtime class-membership
  test expression.
- `as(<class>, value)` is now recognized as a checked object cast/narrowing
  expression.
- Both forms accept semantic `<object>` top values and known nominal object
  values. Primitive values are rejected during semantic lowering.
- Statically impossible nominal checks are diagnosed:
  - `instance?(make(<square>, ...), <circle>)`;
  - `as(<circle>, make(<square>, ...))`.
- The semantic IR records local true-branch refinement for:
  - `if instance?(x, <circle>) then ... else ... end`
- Refined local uses carry their narrowed semantic value category, so
  `<object>` parameters can feed slot reads after an `instance?` guard.
- Lowered IR represents tests and casts with ordinary instructions that emit
  ordinary runtime host calls:
  - `interloko.instance_of`;
  - `interloko.cast_object`.
- No new `execution_tape` generic, cast, or class-test opcodes were added.
- AST, semantic, lowered, and tape dumps expose tests, casts, and refinement.
  Semantic fingerprints include use-site local result kinds and refinements so
  cache reports can see edits that affect narrowing.
- Corpus coverage includes guarded slot access through `<object>`, explicit
  checked casts, impossible nominal tests/casts, and primitive test
  diagnostics.

### Deferred Object Refinement Design

- Refinement is deliberately local and positive only. Else-branch narrowing,
  negative tests, compound boolean conditions, path-sensitive propagation
  across function calls, and generalized local type inference remain deferred.
- `as` is a runtime checked cast from top or nominal object values. It is not a
  general unboxing mechanism for primitive values stored in `<object>`.
- Runtime class tests now follow the single nominal superclass chain.
  Generalized refinement across function calls and compound control flow remains
  deferred.

## Implemented: First-Class Known Callable Objects

- Bare references to direct functions, generic functions, and unambiguous slot
  accessors now lower to semantic known-callable values when no local shadows
  the name.
- Known callables are represented in semantic value categories, so `let`
  bindings can carry identities such as:
  - `function f0`;
  - `generic g0`;
  - `accessor c0.s0`.
- `apply(callable, ...)` is recognized as a narrow built-in call form.
- Applying a known direct function value resolves back to the existing
  semantic `DirectCall` path, including arity and argument-kind checks.
- Applying a known generic function value resolves back to the existing
  semantic `GenericCall` path, including generic contract checks.
- Slot accessor identities are exposed as known callable values, but applying
  them is intentionally diagnosed for now.
- Known callable locals are compile-time-only in lowering. They do not allocate
  lowered registers and do not create runtime `execution_tape` values.
- Lowered IR and tape emission remain unchanged for successful `apply` forms:
  the emitted code is the ordinary direct call or generated generic dispatcher
  call that Interloko already used.
- Semantic dumps and fingerprints expose known-callable identities, including
  local bindings. Incremental coverage shows that changing a callable target in
  one function recomputes the function-level lowered/tape artifacts while
  reusing unrelated method artifacts.
- Corpus coverage includes direct-function `apply`, generic-function `apply`,
  accessor identity as a value, non-callable apply diagnostics, and accessor
  apply diagnostics.

### Deferred Callable Object Design

- There is still no runtime representation for unknown function objects,
  closures, captured environments, dynamic invocation, or accessor invocation
  through `apply`.
- Known callable values cannot cross an ABI boundary or be returned as runtime
  values. They are local compile-time facts consumed by semantic lowering.
- Multi-argument direct-function `apply` is structurally supported by the
  semantic path, but generic dispatch remains limited by the current
  one-argument dispatcher model.

## Implemented: Host-Backed Metaobject Foundation

- `HostMetaRegistry` lets an embedding Rust host declare host-backed classes,
  slots, and functions explicitly. This is metadata only; the registry does not
  inspect Rust values or depend on reflection.
- Host-backed classes and Interloko-native classes share the same nominal
  `ClassId` space. Methods can specialize on either kind, and generic dispatch
  uses the same class-id comparison path for both.
- Semantic classes and slots now carry origin metadata:
  - `native` for Loko source definitions;
  - `host-backed(hcN)` and `host-backed(hcN.hsN)` for registry declarations.
- Semantic dumps show class and slot origin, so it is visible when a program is
  using a Rust-backed metaobject instead of an Interloko-native definition.
- Host-backed slots have Interloko `ValueKind` declarations and are read-only in
  this slice.
- `make(<host-backed-class>, ...)` is rejected during semantic lowering.
  Host-backed object handles must be supplied by the embedding runtime.
- `InterlokoHost` can install host-backed object handles through a
  `HostObjectAdapter`. The adapter owns Rust value access and exposes
  read-only slot reads back to the Interloko runtime.
- Host-backed object handles participate in:
  - `<object>` boxing through the host value representation;
  - `instance?`;
  - `as(<host-backed-class>, value)`;
  - slot reads;
  - generic method dispatch.
- Lowered IR continues to expose ordinary model operations such as class tests,
  casts, generic dispatch calls, and `slot.read` before tape emission.
- Tape emission still uses ordinary runtime host calls such as
  `interloko.slot_get`, `interloko.instance_of`, and `interloko.cast_object`. There are no
  host-backed-specific `execution_tape` opcodes.
- Tests include hand-written Rust adapters for small Rust-backed shapes and
  cover slot reads, generic dispatch, `<object>` boxing, casts, invalid slot
  reads, and invalid casts.
- `examples/host_backed_area` is a runnable Rust embedding example. It creates
  Rust-backed circle and square values, installs them into `InterlokoHost`, and
  runs Interloko generic dispatch over those host-backed object handles.
- `interloko_corpus` supports `// host-class:` headers for compile/dump fixtures,
  and the corpus now includes host-backed slot, dispatch, construction, slot,
  and cast cases.

### Host-Backed Glossary

- **Host-backed class:** a nominal Interloko class declared by the embedding host
  instead of by Loko source.
- **Host-backed slot:** a read-only Interloko slot whose value is provided by a
  host adapter.
- **Host object handle:** a runtime object handle stored in `InterlokoHost` that
  points at adapter-owned Rust data.
- **Adapter:** Rust code that maps an Interloko `SlotId` and expected `ValueKind` to
  a runtime value.
- **Registry:** explicit metadata describing host-backed metaobjects before
  semantic lowering.

### Deferred Host-Backed Design

- `facet` integration is deferred. It should later populate
  `HostMetaRegistry`; it should not replace Interloko's object-model semantics.
- Automatic Rust reflection, mutation, Interloko-side construction of host-backed
  values, ownership/lifetime generality, dynamic unknown host classes,
  host-backed generic Rust types, enum/variant modeling, trait discovery,
  dynamic method installation, persistence, async/effects integration, and
  Cranelift remain out of scope.
- Host-backed functions are metadata scaffolding in this slice. Runtime calls
  still go through the existing Interloko lowering and host-call mechanisms.

## Implemented: MOP Inspector Dumps

- `SessionArtifacts::dump_stage(DumpStage::Mop)` renders a read-only
  metaobject protocol summary from the existing semantic module and runtime
  layout.
- The MOP dump exposes:
  - native and host-backed class origins;
  - slots, declared slot kinds, storage ownership, and runtime representation;
  - accessor identities and the slots they read;
  - generic functions, method specializers, and method result categories;
  - generic dispatch rows from nominal class to method;
  - runtime layout class/slot storage kind.
- `interloko_lab dump <file> --stage mop` and `interloko_lab mop <file>` print the MOP
  summary.
- `interloko_corpus` supports `dump mop contains ...` expectations.
- `interloko_lab` compile-only paths understand leading `// host-class:` headers
  as workbench metadata, matching the corpus runner. This remains a tooling
  convention, not Loko source syntax.

### Migration Notes

- `DumpStage` gained a public `Mop` variant. Exhaustive downstream matches on
  `DumpStage` must handle it.
- CLI stage lists now include `mop`.

## Implemented: Structured Environment Object Protocol Records

- `DESIGN.md` now frames Interloko as a Dylan-shaped semantic kernel for
  executable object systems inside a larger human/agent workspace, with clear
  boundaries around persistence, search, UI, and scheduling.
- `SessionArtifacts::environment_objects()` returns typed EOP records for the
  current compilation artifacts.
- `SessionArtifacts::project_environment()` emits those records through an
  `EnvironmentObjectSink` so tools do not need to parse text dumps.
- `SessionArtifacts::dump_stage(DumpStage::Eop)` renders its text from the
  typed records, preserving the existing workbench/corpus dump behavior.
- The EOP dump is intentionally descriptive. It does not add persistence,
  permissions, UI rendering, search, scheduling, mutation, or a new runtime.
- EOP objects currently cover:
  - source files;
  - classes, slots, accessors, generics, methods, and direct functions;
  - diagnostics;
  - runtime layout rows;
  - query artifacts.
- Each object reports a small protocol vocabulary:
  - `kind`;
  - meta-level `class`;
  - `origin`;
  - `storage`;
  - `presentation`;
  - `affordances`;
  - `execution` when applicable;
  - `inspection`.
- Stable affordance names are modeled by `EnvironmentAffordance`, including
  `inspect`, `dump-syntax`, `dump-ast`, `dump-sem`, `dump-mop`, `dump-eop`,
  `dump-lowered`, `dump-tape`, `show-dispatch`, `show-runtime-layout`,
  `show-dependencies`, `show-accessor`, `show-slot`, `run`, `edit-body`, and
  `add-method`.
- `HostEnvironmentProvider` is an explicit host projection seam. Host adapters
  can contribute host-owned environment records into the same sink without
  making `InterlokoHost` a world registry or enumerating the runtime heap.
- `interloko_lab dump <file> --stage eop` and `interloko_lab eop <file>` print the EOP
  summary. The workbench still understands leading `// host-class:` metadata so
  host-backed objects are visible in the environment view.
- `interloko_corpus` supports `dump eop contains ...` expectations.

### EOP Glossary

- **Environment object:** anything the workbench can identify and act on, such
  as a method, diagnostic, runtime layout row, or query artifact.
- **Origin:** where the object came from: source, host metadata, compiler
  generation, and eventually agent/import sources.
- **Storage:** who owns the current representation: source text, host adapter,
  registry metadata, or cached compiler artifact.
- **Presentation:** stable text hints that a future inspector or transcript can
  render.
- **Affordance:** a named action that a human, agent, or Portolan subject can
  discover and request.
- **Host projection:** an opt-in Rust adapter hook that contributes host-owned
  environment records. It is not runtime heap discovery.

### Deferred EOP Design

- EOP records are Rust structs, not a stable serialized schema. There is no
  Portolan subject bridge, Understory inspector, persistent image, capability
  policy, transaction model, or agent provenance model yet.
- Future Interloko-level generic functions may mirror these projection protocols,
  but this slice keeps projection at the Rust/session API boundary.

### Migration Notes

- `DumpStage` gained a public `Eop` variant. Exhaustive downstream matches on
  `DumpStage` must handle it.
- The session API now exposes `EnvironmentObject`, `EnvironmentObjectId`,
  `EnvironmentKind`, `EnvironmentOrigin`, `EnvironmentStorage`,
  `PresentationFacet`, `AffordanceFacet`, `ExecutionFacet`, `InspectionFacet`,
  `EnvironmentObjectSink`, and `HostEnvironmentProvider`.
- CLI stage lists still include `eop`.

## Implemented: EOP Workbench Commands

- `interloko_lab inspect <file> <object-id>` reads structured EOP records and prints
  one object with kind, class, origin, storage, presentation, affordances,
  execution, and inspection facets.
- `interloko_lab affordances <file> <object-id>` lists the stable affordance names
  exposed by one object and reports unknown object IDs with available IDs.
- `interloko_lab invoke-affordance <file> <object-id> <affordance>` executes a
  read-only workbench action when the object exposes it.
- Supported read-only invocations map to existing compiler/session views:
  `show-slot`, `show-accessor`, `show-dispatch`, `show-runtime-layout`,
  `show-dependencies`, `dump-syntax`, `dump-ast`, `dump-sem`, `dump-mop`,
  `dump-eop`, `dump-lowered`, `dump-tape`, and `run`.
- `tools/interloko_lab/fixtures/host_projected_environment.loko` demonstrates an
  explicit host projection provider with a class-like host object and an
  instance-like host object. The provider is installed only from explicit
  `// host-eop-*` fixture metadata.
- `docs/eop_workbench.md` documents how to discover object IDs, inspect records,
  list affordances, invoke read-only views, and inspect host-projected records.

### Workbench Boundary

- `interloko_lab` is a workbench surface, not the long-term UI.
- Host projection is adapter-owned and explicit. `InterlokoHost` does not enumerate
  runtime heap objects.
- EOP remains structured protocol vocabulary. There is still no Portolan bridge,
  Understory inspector, persistent image, capability policy, mutation
  transaction model, or Interloko-level EOP generic function protocol.

## Implemented: Richer Dylan-Shaped Slots

- Slot declarations now support:
  - untyped slots defaulting to `<object>`;
  - typed slots such as `slot radius :: <integer>;`;
  - renamed init keywords such as `init-keyword: r:`;
  - required init keywords such as `required-init-keyword: radius:`;
  - default initializer expressions with `init-value: <expr>`;
  - `constant slot` metadata.
- Semantic direct slots now record declared type, normalized value category,
  init keyword, required flag, optional default initializer expression, instance
  allocation metadata, mutability metadata, origin, and stable accessor
  identity.
- Object construction now fills omitted slots from `init-value:` expressions.
  Explicit keyword values override defaults. Required init keywords may be
  omitted only when a default exists.
- Construction diagnostics cover unknown init keywords, duplicate init
  keywords, missing required keywords, missing non-defaulted slot initializers,
  explicit initializer type mismatches, and `init-value:` type mismatches.
- `constant slot` is metadata only in this slice. It records construction-time
  assignment intent, but reads lower through the same accessor/slot-read path
  as mutable slots because setters and mutation are not implemented yet.
- MOP dumps, EOP records, and `interloko_lab inspect ... slot:*` now show slot
  type, init keyword, required/default status, allocation, mutability, runtime
  representation, and accessor relationship.
- Incremental fingerprints include slot defaults and slot metadata. Tests cover
  harmless slot declaration trivia preserving definition IDs, init keyword edits
  reusing executable artifacts when layout is unchanged, default-value edits
  recomputing lowered/tape artifacts, and mutability metadata edits reusing
  lowered/tape artifacts.
- The corpus includes pass cases for renamed keywords, `init-value:`, default
  override, required keywords with defaults, object-top defaults, and constant
  slots, plus a failing `init-value:` type mismatch case.
- `docs/slot_model.md` documents supported slot syntax, construction behavior,
  workbench inspection, and deferred full initialization/mutation design.

### Deferred Slot Design

- Slot writes/setters, inherited slot specifications, class allocation, each-subclass
  allocation, virtual/computed slots, full Dylan initialization protocol hooks,
  before/after/around init methods, sealed slot optimization, persistence/image
  storage, host-backed construction, and generalized mutability enforcement are
  still deferred.

## Implemented: AMOP-Informed Class Finalization And Inherited Slots

- AST lowering now preserves the explicit superclass written in
  `define class <child> (<parent>)`. Omitting the superclass list still
  normalizes to the `<object>` root.
- Semantic classes now distinguish:
  - `direct_slots`, which are source-authored facts;
  - effective slots, which are computed finalization facts.
- Class finalization computes effective slots in deterministic layout order:
  parent effective slots first, then the class's direct slots.
- Inherited effective slots preserve the original direct slot/accessor
  definition identity while receiving a subclass-local layout slot. This keeps
  definition IDs stable while allowing the runtime to read the correct child
  object field.
- `make(<child>, ...)` initializes all effective slots. Inherited init keywords,
  required keywords, and `init-value:` defaults work the same way as direct
  slots, and explicit keyword values still override defaults.
- Slot reads and explicit accessor calls resolve against the receiver class's
  effective slots, so `child.parent_slot` and `parent_slot(child)` work for
  inherited slots.
- Finalization diagnostics reject unknown superclasses, primitive
  superclasses, host-backed superclasses, duplicate effective slot names,
  duplicate effective init keywords, and duplicate effective accessors.
- Runtime layout is derived from effective slots. `execution_tape` still sees
  ordinary construction/read host calls; no inheritance-specific tape opcodes
  were added.
- Semantic/MOP dumps and EOP inspection now show superclass, finalization
  status, direct slots, effective slots, direct vs inherited source, source
  slot identity, and effective layout order.
- Incremental tests cover harmless superclass/slot trivia, parent slot edits
  affecting child finalization and executable artifacts, child-only slot edits
  leaving parent definitions unchanged, and adding a subclass while preserving
  parent definition IDs.
- Corpus coverage includes inherited construction/read cases, inherited
  `init-value:`, inherited required keywords, parent+child slots, inherited
  accessor calls, constant inherited slots, and expected superclass/conflict
  diagnostics.
- `docs/mop_class_finalization.md` explains the AMOP-inspired vocabulary and
  the boundary between source facts, computed MOP facts, runtime layout, and
  exposed workbench/EOP views.

### Deferred Class-Finalization Design

- AMOP is vocabulary and architecture guidance here, not CLOS compatibility.
- Dylan inherited slot specifications, user-defined `initialize` methods, and
  full initialization protocol hooks remain deferred.
- Multiple inheritance, class precedence lists, public metaclass protocols,
  user-defined metaclasses, class redefinition, instance obsolescence, method
  combination, setters, sealed slot optimization, persistence, host-backed
  construction, and generalized mutability enforcement remain deferred.

### Migration Notes

- `sem::Class::slots` was renamed to `sem::Class::direct_slots`. Code that
  wants source-authored slot declarations should use `direct_slots`; code that
  wants construction/read/runtime layout should use `effective_slots`.

## Implemented: Nominal Inheritance Semantics And Inspection

- Single inheritance now acts as a nominal subtype relation. Semantic checking
  accepts subclass instances where a parent object is expected for slot
  initialization, direct function parameters/returns, generic call arguments,
  generic method specializers, casts to a parent, and `instance?` tests against
  a parent.
- Restart matching now uses the same relation when a signaled condition is a
  subclass of the active restart condition class. The restart condition binding
  keeps the restart's parent-specializer type.
- Runtime `instance?`, `as`, and object slot storage checks use the emitted
  runtime superclass chain, so parent-typed values work after lowering to tape.
- Generic dispatch now emits ordinary lowered control flow that checks runtime
  class IDs and selects the most-specific applicable method in the single
  superclass chain. There are still no inheritance-specific execution_tape
  opcodes.
- Disjoint sibling classes remain incompatible for statically known
  `instance?` and `as` operations.
- Incremental fingerprints for generic dispatchers include dispatch order and
  method applicability, so adding or editing subtype-relevant definitions
  affects the right artifacts.
- Corpus coverage now includes parent-typed slots with child values, direct
  calls that upcast child values, parent-specialized methods handling child
  instances, most-specific subclass dispatch, subclass condition restarts, and
  sibling impossible-test/cast diagnostics.
- MOP, semantic, EOP, and runtime-layout inspection now report superclass
  chains, direct/effective/runtime slot source, and dispatch applicability. The
  workbench fixtures `inherited_point.loko`, `inherited_dispatch.loko`, and
  the inherited parent-slot diff fixtures show how to inspect inherited
  behavior and incremental propagation.
- `docs/mop_class_finalization.md` and `docs/slot_model.md` describe the
  subtype semantics, inspection commands, artifact propagation, and the
  remaining deferred design.

## Implemented: Explainable Generic Dispatch-Site Inspection

- `SessionArtifacts::dispatch_sites()` now walks semantic functions and methods
  and records every `GenericCall` as an inspectable dispatch site.
- Dispatch sites have friendly IDs such as `dispatch:d0` plus stable backing
  keys derived from the source file, enclosing definition, owner-local call ID,
  and generic definition. The record also carries the current source span for
  navigation.
- `SessionArtifacts::explain_dispatch_site("dispatch:d0")` reports the
  enclosing function/method, span, generic contract, first argument's static
  kind, exact class when known, applicable methods in dispatcher specificity
  order, rejected methods with reasons, selected method when statically known,
  the lowered dispatcher name, and related artifact keys.
- Parent-typed and top-object calls are reported as runtime-needed rather than
  pretending the selected method is statically knowable.
- `interloko_lab dispatch <file>` lists dispatch sites, and
  `interloko_lab dispatch <file> dispatch:d0` explains one site.
- EOP projection now includes `dispatch:dN` objects. `inspect <file>
  dispatch:d0` shows the structured record, and `invoke-affordance <file>
  dispatch:d0 show-dispatch` prints the same dispatch explanation.
- Tests cover exact class selection, child-over-parent selection,
  parent-typed runtime dispatch, rejected sibling methods, stable dispatch-site
  IDs across harmless trivia edits, and method/class fact changes affecting the
  expected explanation and artifacts.
- `docs/eop_workbench.md` and `docs/mop_class_finalization.md` document how to
  list and inspect dispatch sites.

### Dispatch-Site Boundaries

- Dispatch-site inspection is read-only and explains the existing semantic and
  lowered model.
- Runtime dispatch is still ordinary lowered control flow plus ordinary calls;
  no execution_tape dispatch opcode was added.
- Multi-argument dispatch, method combination, sealed dispatch optimization,
  parametric dispatch, and a public AMOP-level dispatch protocol remain
  deferred.

### Migration Notes

- `EnvironmentKind` has a new `DispatchSite` variant with stable text label
  `dispatch-site`.

## Implemented: Initialization Plans For Dylan-Style Initialize Substrate

- Each finalized semantic class now has an `InitializationPlan` derived from its
  effective slots. The plan records deterministic construction order, source
  slot identity, accepted init keyword, value kind/type name, inherited/direct
  source, requiredness, default presence, and explicit-argument override
  behavior.
- Semantic `make(<class>, key: value, ...)` now records explicit `InitArg`
  entries at the call site before choosing planned field initializers. The
  chosen field initializers distinguish explicit values from default
  `init-value:` expressions.
- Construction validation now separates unknown keywords, duplicate keywords,
  missing required keywords, explicit initializer type mismatches, and default
  `init-value:` type mismatches.
- Initialization order is inherited effective slots in layout order, followed
  by direct effective slots in layout order. Explicit init args override
  defaults, and required slots without explicit/default values diagnose.
- Lowered IR object construction consumes the class initialization plan rather
  than iterating effective slots ad hoc. The execution_tape path remains
  ordinary runtime object construction and host calls; no initialization or
  object-aggregate tape opcode was added.
- Semantic dumps, MOP dumps, EOP class records, and `interloko_lab inspect <file>
  class:cN` expose initialization plans, accepted keywords, required/default
  status, slot order, and explicit-argument override behavior.
- Inspection explicitly reports `initialize-methods=deferred`. This slice is
  not avoiding Dylan-style initialization; it is the substrate future
  `make`/`initialize` semantics can consume.
- Tests cover inherited initialization order, explicit overrides, required
  inherited keywords, unknown/duplicate keywords, explicit and default
  initializer type mismatches, stable init facts across trivia edits, and
  expected artifact changes after slot init metadata edits.
- `docs/mop_class_finalization.md`, `docs/slot_model.md`, and
  `docs/eop_workbench.md` document the current init-plan model, how to inspect
  it, how it maps toward Dylan `make`/`initialize`, and why user-defined
  `initialize` remains deferred.

### Deferred Initialization Design

- User-defined `initialize` methods, `make` as an overridable generic function,
  custom allocation protocols, keyword forwarding beyond slot initialization,
  `next-method`, method combination, reinitialization, class redefinition,
  obsolete instances, slot setters, multiple inheritance, and host-backed
  construction remain deferred.
- Before user-defined initialization is enabled, Interloko still needs decisions
  about method ordering, keyword consumption/forwarding rules, allocation hooks,
  interaction with setters/mutation, and how initialization participates in MOP
  inspection and incremental invalidation.

## Implemented: Catalog-First Diagnostic Model

- `interloko_compiler::diagnostic::Diagnostic` now carries stable diagnostic facts:
  code, severity, span, labels, structured args, and notes. It does not carry a
  rendered English message.
- `DiagnosticCode` provides stable code strings such as
  `sem.unresolved-local`, `sem.missing-required-init-keyword`,
  `sem.slot-initializer-type-mismatch`, `syntax.expected-expression`,
  `lower.invalid-semantic`, `tape.build-error`, and `runtime.trap`.
- Lowering, tape emission, driver/runtime, and session runtime outcomes expose
  stable error codes for non-source diagnostic failures.
- Diagnostic fingerprints and logical work counters now include structured
  code/arg/label facts instead of rendered prose.
- EOP diagnostic records expose code, severity, span, labels, and args, without
  rendered text. `interloko_lab inspect <file> diagnostic:dN` shows those fields,
  and CLI diagnostic printing includes the code beside catalog-rendered text.
- `tools/interloko_diagnostics` owns MessageFormat catalog loading and rendering.
  Rendering is strict: missing catalog entries, missing required args, and
  MessageFormat failures are presentation errors rather than silent fallbacks.
- Built-in English and pseudo catalogs cover every stable diagnostic code.
  `interloko_lab` selects them with `INTERLOKO_DIAGNOSTIC_LOCALE=en|pseudo`, or loads an
  override TOML resource through `INTERLOKO_DIAGNOSTIC_CATALOG`.
- `interloko_corpus` supports `// expect: diagnostic code ...` while keeping
  `diagnostic contains ...` expectations for catalog-rendered English checks.
- Corpus coverage now includes representative code expectations for syntax,
  AST, semantic, control-flow, restart, and initialization diagnostics.
- `docs/diagnostics.md` explains the boundary between diagnostic facts and
  presentation, how to add/override catalogs, and why artifact identity is
  language-neutral.

### Deferred Diagnostic Design

- Full localization beyond English and pseudo, a `message-format` dependency in
  `interloko_compiler`, terminal snippet rendering, LSP adaptation, fix-it edits,
  warning severities, and generated diagnostic schemas remain deferred.

## Implemented: First Richer Value Types

- Interloko now parses and lowers string, symbol, integer, and immutable vector
  literals:
  - strings use `"..."` with `\n`, `\t`, `\"`, and `\\` escapes;
  - symbols use `#name`;
  - keyword-style values use Dylan-style `name:` and lower to symbols; the
    older `#:name` spelling remains accepted as a legacy symbol spelling;
  - integers support prefix negation, including `i64::MIN`;
  - vectors use Dylan-style `#[a, b, c]`.
- Semantic IR has explicit `ValueKind` coverage for `<string>`, `<symbol>`,
  raw integers, and `<vector>`. `<string>`, `<symbol>`, `<integer>`, and
  `<vector>` have built-in class metaobjects; `<integer>` still lowers as a raw
  `i64` at the execution boundary.
- Symbols are interned by the runtime host for stable per-session identity.
  Strings are runtime objects compared by content. Vectors are immutable ordered
  runtime objects; general sequence operations remain deferred.
- Lowered IR contains inspectable literal instructions and runtime value
  equality. Tape emission still uses ordinary constants, calls, and host calls;
  there are no special value-type opcodes.
- Slots, direct functions, generic contracts, and method specializers can use
  `<string>`, `<symbol>`, `<integer>`, and `<vector>` annotations.
- Session, MOP, EOP, lowered, and tape dumps expose the new built-in value
  classes and literal forms.
- `examples/rich_values`, `tools/interloko_lab/fixtures/rich_values.loko`, REPL
  transcript coverage, and corpus programs demonstrate strings, symbols,
  keyword-style symbols, vectors, typed slots/functions, integer specializers,
  and generic dispatch.
- Diagnostics cover malformed string literals and unsupported vector equality
  through structured diagnostic codes.

### Deferred Value-Type Design

- Hash tables, a sequence library, mutable vectors, string interpolation,
  Unicode normalization, locale-sensitive string behavior, reader macros,
  package-qualified symbols, persistent cross-session interning, and optimizer
  passes remain deferred.

## Intentionally Deferred

- Sealed dispatch optimization.
- Parametric types.
- Effects and restarts.
- Generalized refinements beyond local positive `instance?` true branches.
- Macros.
- Cranelift.
- Full module system.
- General type checking beyond the current primitive, top object, nominal
  object, and slot value categories.
- Multi-argument/multi-method dispatch beyond one nominal object argument.
- Multiple inheritance, method combination, sealed dispatch optimization, and
  parametric types.
- Runtime mutation.
- Persistent caches or scheduling; the invalidation shell models dependencies only.
- Watch mode, LSP, and persistent on-disk workbench caches.
- Full multi-file module sessions beyond compiling named source files independently.
- Cross-file/module-stable identity beyond caller-provided `SourceFileId` values.
- Lowered IR optimization passes, canonical basic blocks, and richer type checking.
- Wall-clock artifact timing in the no-std core; the current engine reports
  deterministic logical ticks and work units.
- Query scheduling that avoids recomputing prerequisite fingerprints when
  validating cache reuse.
- Cross-call condition signaling, dynamic handler stacks, and effect typing.
- Automatic host reflection through `facet`.
- Host-backed mutation, construction, generic Rust types, enum/variant
  modeling, trait discovery, and dynamic host class installation.

## Follow-Up Tickets

- `cai-8lcw`: define stable compiler ID strategy.
- `cai-f4sj`: split lowered IR from tape emission (implemented by `cai-uv89`).

## Validation

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `cargo doc --workspace --all-features --no-deps`
- `cargo run -p interloko_forty_two_example` prints `42`.
- `cargo run -p interloko_generic_area_example` prints `100`.
- `cargo run -p interloko_host_backed_area_example` prints `74`.
- `cargo run -p interloko_corpus` checks the small-program corpus.
