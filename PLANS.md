# Plans

## Make Slots Dylan-Shaped

Ticket: `cai-lnet`

### Goal

Make Cairn slot declarations follow the useful part of Dylan's slot shape:
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
<object>             => top Cairn value category
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
   can store and return any first-slice Cairn value.
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
slot values for runtime-managed Cairn objects. Neither side owns native aggregate
layout in this slice.

### Invariants

- Untyped legacy `slot name;` declarations remained accepted in this slice.
  The later `cai-lnet` slot-initialization slice changed their default to
  `<object>`.
- Explicit primitive slot types are limited to `<integer>` and `<boolean>`.
- Explicit nominal slot types refer to already-declared or source-declared Cairn
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

Add Cairn's first recoverable condition/restart vertical slice. Condition
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
   - `cairn_lab` fixture tests;
   - `cairn_corpus`.

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

Add Cairn's first expression-level control-flow slice: boolean literals,
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
   - `cairn_lab` fixture tests;
   - `cairn_corpus`.

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
  -> cairn_lab edit output + cairn_corpus checks
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
5. Add `cairn_lab` output for edit reports.
6. Update `cairn_corpus` so corpus checks compile through the incremental
   engine.
7. Update progress notes and migration notes.
8. Run full gates:
   - `cargo fmt --all`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace --all-features`;
   - `cargo doc --workspace --all-features --no-deps`;
   - existing examples;
   - `cairn_lab` fixture commands;
   - `cairn_corpus` runner.

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

Build a small, durable corpus of `.cairn` programs and a workspace runner that
validates each program through the session API. The corpus should make it easy
to add new semantic examples, expected diagnostics, and lightweight dump
stability checks without committing large snapshot files.

### Non-Goals

- No new language semantics.
- No new production dependencies.
- No persistent snapshot store.
- No watch mode or editor integration.
- No replacement for `tools/cairn_lab`; this runner is batch validation, not an
  interactive workbench.

### Fence

The `corpus/` tree owns source fixtures and declarative expectations; it
explicitly does not own compiler behavior or dump formatting.

The `tools/cairn_corpus` crate owns filesystem traversal, expectation parsing,
session invocation, and human-readable check output; it explicitly does not own
syntax, semantic, lowering, tape, or runtime rules.

### Plumbing

```text
corpus/**/*.cairn
  -> header expectations
  -> tools/cairn_corpus
  -> cairn_compiler::session
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
2. Add `tools/cairn_corpus` with:
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
   - `cairn_lab` fixture commands;
   - `cairn_corpus` runner.
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

Build a usable incremental compiler workbench around the existing Cairn
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

The `tools/cairn_lab` crate owns file IO, CLI parsing, process exit behavior,
and presentation; it explicitly does not own compiler state or semantic
decisions.

### Plumbing

```text
.cairn file(s)
  -> cairn_compiler::session
  -> syntax / ast / sem / lower / tape / runtime
  -> dumps + artifact graph + edit diff
  -> tools/cairn_lab CLI
```

### Steps

1. Add `session` API to `cairn_compiler`:
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
4. Add `tools/cairn_lab` CLI:
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
  `tools/cairn_lab`.
