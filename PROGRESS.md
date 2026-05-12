# Cairn Progress

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

- Backend-neutral lowered IR in `cairn_compiler::lower`.
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
- Stability guarantees and non-guarantees are documented in `crates/cairn_compiler/ADR-stable-ids.md`.

## Implemented: Compiler Session and Workbench CLI

- `cairn_compiler::session` provides a no-std-friendly compiler session API over named source files.
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
- `tools/cairn_lab` provides:
  - `run <file>`
  - `dump <file> --stage syntax|ast|sem|lowered|tape|all`
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
- Each `.cairn` corpus file declares expectations in leading comments:
  - `// expect: run N`;
  - `// expect: diagnostics N`;
  - `// expect: diagnostic contains TEXT`;
  - `// expect: dump STAGE contains TEXT`.
- Dump expectations are intentionally substring-based so they act like small
  semantic landmarks rather than full brittle snapshots.
- `tools/cairn_corpus` provides:
  - `check [corpus-root]` to compile every corpus program through
    `cairn_compiler::session`;
  - `list [corpus-root]` to inspect discovered cases;
  - `dump <file.cairn> --stage syntax|ast|sem|lowered|tape|all` for
    snapshot-like stage output without committing generated dumps.
- Add a new case by placing a `.cairn` file under `corpus/pass` or
  `corpus/fail`, adding at least one `// expect:` header line, and running
  `cargo run -p cairn_corpus`.

## Implemented: In-Memory Incremental Compiler Engine

- `cairn_compiler::session::IncrementalCompilerSession` provides an in-memory
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
- `cairn_lab diff <before> <after>` now runs through the incremental session
  and prints cache status, counters, dependency edges, and the semantic diff.
- `cairn_corpus check` now compiles each program through
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
- `cairn_lab diff` output is now an incremental cache report followed by the
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
  operations. There are no special Cairn control-flow opcodes.
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

## Intentionally Deferred

- Sealed dispatch optimization.
- Parametric types.
- Effects and restarts.
- Refinements.
- Macros.
- Cranelift.
- Full module system.
- General type checking beyond the current `I64` and nominal object value categories.
- Multi-argument/multi-method dispatch beyond one nominal object argument.
- Runtime mutation and non-integer slots.
- Persistent caches or scheduling; the invalidation shell models dependencies only.
- Watch mode, LSP, and persistent on-disk workbench caches.
- Full multi-file module sessions beyond compiling named source files independently.
- Cross-file/module-stable identity beyond caller-provided `SourceFileId` values.
- Lowered IR optimization passes, canonical basic blocks, and richer type checking.
- Wall-clock artifact timing in the no-std core; the current engine reports
  deterministic logical ticks and work units.
- Query scheduling that avoids recomputing prerequisite fingerprints when
  validating cache reuse.

## Follow-Up Tickets

- `cai-8lcw`: define stable compiler ID strategy.
- `cai-f4sj`: split lowered IR from tape emission (implemented by `cai-uv89`).

## Validation

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `cargo doc --workspace --all-features --no-deps`
- `cargo run -p cairn_forty_two_example` prints `42`.
- `cargo run -p cairn_generic_area_example` prints `100`.
- `cargo run -p cairn_corpus` checks the small-program corpus.
