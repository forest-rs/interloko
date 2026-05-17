# Interloko Progress

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
