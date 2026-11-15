# Interloko EOP Mutation

## Overview

Most EOP affordances are read-only: they project compiler facts as
inspectable records. The executable mutation affordances are `add-method`,
which inserts a new method into an existing generic in a running
compiler+runtime session, and `annotate`, which writes a type annotation on
a binding or result and reports what typing it changed (see
[Annotate](#annotate)).

The mutation is the first proof that the EOP record surface is a real
protocol substrate and not only an inspection surface. Future mutation
slices (`retract-method`, `edit-body`, `define-class`, and eventually a
session restore path) compose with the same machinery.

## Boundary

Compiler sessions own structured mutation requests, provenance, candidate
construction, rollback, diagnostics, and artifact-graph impact reporting.
`interloko_runtime` owns executable method-table mutation, dispatch
behavior, and the layout invariants that mutation must preserve.
`interloko_lab` owns the user-facing executable affordance surface and
result rendering.

This slice explicitly does not own:

- persistent images, on-disk session storage, or any session
  serialization format. Mutations are in-memory only;
- mutation affordances other than `add-method` and `annotate`. No
  `retract-method`, `edit-body`, `define-class`, or `redefine-class`;
- object migration, slot reshape, generic-signature change, or any
  class-graph mutation;
- multi-session collaboration, locking, or capability policy;
- a new text-as-structure escape hatch beyond the affordance's text
  input. Method source is parsed immediately through the ordinary
  parser into structured AST;
- cache reuse. The implementation does a full candidate-and-swap
  recompute on every mutation. See "Recompute strategy" below.

## In-memory fence

Sessions are mutated in place; nothing is written to disk. No dump or
report path is repurposed as implicit serialization. CLI rendering of
mutation outcomes describes the mutation that just happened in this
session, not a record that survives session boundaries.

This boundary is load-bearing for the larger Interloko/Vivarium framing:
persistence belongs in a future image/world layer, not in the
mutation-affordance contract. Crossing the fence later requires a
separate plan and an explicit serialization design.

## API shape

```rust
pub enum SessionMutation {
    AddMethod {
        generic: sem::GenericFunctionId,
        method_source: String,
    },
    Annotate {
        target: AnnotationTarget,
        type_source: String,
    },
}

pub struct MutationOutcome {
    pub mutation_id: u32,
    pub effect: MutationEffect,
    pub affected_artifacts: Vec<QueryArtifact>,
    pub recompute_strategy: RecomputeStrategy,
}

pub enum MutationEffect {
    InsertedMethod(sem::MethodId),
    Annotated(Box<AnnotationOutcome>),
}

pub enum RecomputeStrategy {
    FullCandidateRebuild,
}

pub enum MutationError {
    ParseFailed { diagnostics: Vec<Diagnostic> },
    NotAMethodDefinition,
    UnknownGeneric { generic: sem::GenericFunctionId },
    GenericNameMismatch { generic, expected, actual },
    SemanticFailed { diagnostics: Vec<Diagnostic> },
    DuplicateSpecializer { existing_method, specializers },
    RuntimeValidationFailed { error: RuntimeLayoutError },
    SealedGeneric { generic },
    UnknownAnnotationTarget { target: AnnotationTarget },
    InvalidTypeSource { type_source: String },
}

impl SessionArtifacts {
    pub fn apply_mutation(
        &mut self,
        mutation: SessionMutation,
    ) -> Result<MutationOutcome, MutationError>;
}
```

`method_source` is a full Loko `define method ...; end;` clause. It is
parsed immediately into structured AST through the ordinary parser; the
mutation request is structured before semantic validation runs.

## Validation order

For `add-method`, `apply_mutation` runs these checks in order. Each check produces a
specific `MutationError` variant so the affordance does not collapse
distinct failure modes into a generic compile failure.

1. The target `generic` id exists in the current sem module
   (`UnknownGeneric`).
2. The candidate source compiles. Syntax-level diagnostics surface as
   `ParseFailed`.
3. The candidate AST gained exactly one new method and no new classes,
   generics, or functions (`NotAMethodDefinition`).
4. The new method's resolved generic matches the requested target
   (`GenericNameMismatch`).
5. The new method's specializer vector does not exactly match an
   existing method on the same generic (`DuplicateSpecializer`). This
   check runs before the general semantic-error check because sem
   already reports such exact duplicates as ambiguity; the affordance
   surfaces the more specific failure mode.
6. No other new semantic diagnostics (`SemanticFailed`).
7. The candidate runtime layout validates
   (`RuntimeValidationFailed`).

On any failure the live session is left in its pre-mutation state.
Rollback is achieved by computing the candidate state in a local
variable and only swapping it into the live session on success.

## Provenance

Successfully inserted methods carry
`sem::MethodOrigin::Session { sequence }`. The sequence number is the
per-session monotonic mutation id. EOP records use this to distinguish
source-origin methods, compiler-synthesized slot setters, and
session-inserted methods.

Session-origin methods participate in dispatch identically to
source-origin methods. The only externally observable difference is the
origin marker exposed in workbench projections.

Provenance is computed from the mutation log, not from method-count
arithmetic. An add-method entry's `outcome.inserted_method()` names the
sem method id that the mutation introduced; the candidate-and-swap retag
walks the log on every rebuild and reasserts
`MethodOrigin::Session { sequence: entry.mutation_id }` for each such
entry, so an earlier inserted method does not silently revert to
`Source` when a later mutation rebuilds the candidate from the
appended source.

## Mutation log

The session holds an in-memory mutation log:

```rust
pub struct AppliedMutation {
    pub mutation_id: u32,
    pub request: SessionMutation,
    pub outcome: MutationOutcome,
}

impl SessionArtifacts {
    pub fn mutations(&self) -> &[AppliedMutation];
}
```

Each successful `apply_mutation` appends exactly one entry; failed
mutations append none. The log is ordered by application. An entry's
`mutation_id` equals the `sequence` carried by the inserted method's
`MethodOrigin::Session`, but an entry's *position* in
`mutations()` is *not* in general equal to its `mutation_id`: after a
rewind, ids are stable session event ids and the log may carry gaps
where dropped ids used to be. See the **Rewind** section below for the
full story.

The log is the single source of truth for session-mutation provenance.
Any earlier code that derived provenance from method-count arithmetic
has been removed. If a caller hand-edits the origin field on a
session-inserted method, the next candidate-and-swap rebuild restores
the marker from the log entry.

Each entry projects as an EOP environment object:

```text
object mutation:m0
  kind=mutation meta-class=<mutation>
  origin=session storage=transient-in-memory
  inspection: mutation-id=0 request=add-method target=<area>
              inserted=<method-id> affected-artifacts=13
              strategy=full-candidate-rebuild
object mutation:m1
  inspection: mutation-id=1 request=annotate target=binding:def…:s
              annotation=<circle> previous=<shape> changed-decisions=2
              affected-artifacts=14 strategy=full-candidate-rebuild
```

The `transient-in-memory` storage label is the contract: the log lives
only in the running session. No part of the slice writes log content
to disk, embeds it in dump or report output as serialization, or uses
it as a session resume input. CLI and EOP wording for log entries does
not say "saved", "stored", or "reused"; the strategy label always
names what actually happened in the implementation.

`interloko_lab invoke-affordance ... add-method` reports the
post-mutation log size on the outcome line as `log_size=<N>`. The lab
is stateless per invocation, so a successful add-method today always
shows `log_size=1`; the field exists so the log is visible from the
CLI and so future stateful surfaces can report cumulative log size
with the same wording.

## Rewind

Rewind truncates the live mutation log at a target id and rebuilds
session state from the pristine source plus the retained entries:

```rust
pub struct RewindOutcome {
    pub retained_count: usize,
    pub dropped: Vec<AppliedMutation>,
    pub recompute_strategy: RecomputeStrategy,
}

pub enum RewindError {
    TargetOutOfRange { target: u32, current_count: usize },
    RuntimeValidationFailed { error: RuntimeLayoutError },
}

impl SessionArtifacts {
    pub fn rewind_to(&mut self, target: Option<u32>) -> Result<RewindOutcome, RewindError>;
}
```

`Some(id)` requires `id` to name an entry currently in the live log;
on success that entry and every earlier entry are retained, and the
strict suffix is dropped. A target that does not name a live entry
(either because it was never assigned, or because it was dropped by
a prior rewind) returns `RewindError::TargetOutOfRange`; rewind
never silently re-interprets a missing id as "retain everything with
a smaller numeric value." `None` retains no entries and returns the
session to its pristine state. A no-op rewind (target names the
current tail) succeeds without recompiling; the outcome's
`recompute_strategy` is still `FullCandidateRebuild` because the
strategy enum is currently coarse, not because work was done. A
finer-grained `Noop` variant is a follow-up.

**Rewind is not a `SessionMutation`.** The mutation log stays a forward
transcript of domain mutations; rewind is an operation *on* that
transcript and is never appended to it. The truncated tail is reported
in `RewindOutcome::dropped` but does not become a log entry. This keeps
the invariant "replaying the log against the pristine source reproduces
current state" intact: a transcript that included rewind entries would
require entries that erase prior entries, breaking the log's meaning.

Mutation ids are stable session event ids, not positional indices.
`SessionArtifacts` carries a private monotonic counter that advances
on every successful `apply_mutation` and is *preserved* across every
`rewind_to`. Retained entries keep their `mutation_id`; dropped ids
are permanently retired. The next `apply_mutation` after a rewind
gets the next never-used id (one past the highest ever assigned),
*not* a recycled dropped id and *not* `retained_count`. The post-
rewind log can carry a deliberate gap where the dropped id used to be.

Concretely: apply two mutations producing ids `[0, 1]`, rewind to
`Some(0)` to drop id 1, then apply again. The next id is `2` (id 1
was retired); the live log is `[0, 2]`; provenance is
`Session { sequence: 0 }` on the retained method and
`Session { sequence: 2 }` on the new one. `rewind_to(Some(1))` after
that point would fail with `TargetOutOfRange { target: 1 }` because
id 1 is no longer in the log.

`compose_source_from_log` is the source-side rebuild seed: it takes
the pristine base and a slice of applied entries and deterministically
produces the composed source text. An add-method entry appends its method
source; an annotate entry applies the edit its outcome recorded, whose span
is in the coordinates of the source composed before it. Recorded edits are
exact only against the base they were made on, which is why replay against
a changed base does not compose: it applies each request again, resolving
annotation targets by their stable ids. The helper does not use ids.

Rewind reuses the same candidate-and-swap discipline as
`apply_mutation`: a candidate is built and validated; on success the
swap is atomic; on failure the live session is unchanged. The
candidate composition shares the internal
`compose_source_from_log(pristine, entries)` helper with the apply path.
An annotate entry whose recorded edit does not fit the composed source
fails with `RewindError::CompositionFailed`; like a layout failure, that
signals a bug rather than an ordinary failure mode.

Rewind is not exposed through `interloko_lab` in this slice. The lab
is stateless per invocation, so a standalone rewind has no useful CLI
surface; tests in `interloko_compiler` are the verification. A
stateful session subcommand is deferred to a future slice along with
the rest of the "long-lived session" UX.

## Replay

Replay turns the mutation log from a session-local history into a
portable transcript:

```rust
pub struct ReplayOutcome {
    pub applied: Vec<AppliedMutation>,
    pub failed: Option<ReplayFailure>,
    pub artifacts: SessionArtifacts,
}

pub struct ReplayFailure {
    pub mutation_id: u32,
    pub log_index: usize,
    pub request: SessionMutation,
    pub cause: MutationError,
}

impl SessionArtifacts {
    pub fn replay_against(&self, new_source: &SourceFile) -> ReplayOutcome;
}
```

`replay_against` builds an ephemeral session from `new_source`, walks
the live mutation log in order, applies each entry's request through
the existing candidate-and-swap discipline using the entry's *original*
mutation id, and stops at the first failure. The returned
`ReplayOutcome` carries the entries that applied (in order), the first
failure (if any), and the ephemeral session's artifacts. The caller
may inspect `artifacts.sem` / `artifacts.tape`, adopt them as a new
session, or discard them; the original session is unchanged either
way.

**Prefix replay only.** Replay does not silently skip entries that
fail to apply. A skip-failed mode would change the meaning of a
transcript — "this log applies modulo these gaps" is a different
contract from "this log applies in order, up to this point" — and
would quietly conflate replay with arbitrary conflict-resolution
policy. A later slice can introduce explicit skip / strict / drop
modes if there is a real need; until then, callers that want to
recover from a partial failure can `rewind_to` and try again on the
adopted artifacts.

**Original ids are preserved.** Applied entries keep their original
`mutation_id`. The ephemeral session's `MethodOrigin::Session`
markers carry the same `sequence` values they had on the source log.
The ephemeral session's `next_mutation_id` is finalized as
`max(applied_ids) + 1`, so subsequent `apply_mutation` calls on the
adopted artifacts never collide with replayed ids and never pretend
the new mutation is older than the replayed history.

**The original session is not mutated.** `replay_against` takes
`&self`. The original log, pristine source, sem module, lowered
module, and tape are all byte-for-byte unchanged regardless of replay
outcome. The `ReplayOutcome::artifacts` field is the only new state.

Replay does not compose recorded source edits: it applies each request
to the new base through `apply_mutation`'s own path, so an add-method
appends to the new base and an annotate finds its target there by stable
id (`binding:<definition>:<name>`), which survives edits to other
definitions.

Concrete failure modes today are the ordinary apply-time
[`MutationError`] variants: `UnknownGeneric` when the target generic
no longer resolves at the original id; `GenericNameMismatch` when an
id now points at a different-named generic; `SemanticFailed` when a
specializer class is gone; `DuplicateSpecializer` when the new base
already defines the method the mutation was adding;
`UnknownAnnotationTarget` when the new base no longer binds the
annotated name in that definition; and so on. The
slice does not try to "rebind by name" when ids drift; that would
be a different (and richer) contract.

Replay is not exposed through `interloko_lab` in this slice. The lab
is stateless per invocation, so a standalone replay has no useful CLI
surface; tests in `interloko_compiler` are the verification.

## Transcript

A `SessionTranscript` makes the mutation log durable: a base source
plus the ordered sequence of mutation events plus the session's
`next_mutation_id` at capture time. Restoration is
`compile(base) + replay(events)` through the existing strict-replay
machinery; the live image stays ephemeral, only the transcript is
durable.

The framing is the conservative version of three well-known patterns:
write-ahead log without checkpoints, event sourcing with a single
event type, and the minimal git-init / git-clone for a programmable
image. The transcript records history, not a snapshot of derived
runtime state.

```rust
pub struct SessionTranscript {
    pub format_version: u32,
    pub source_file_name: String,
    pub base_source: String,
    pub next_mutation_id: u32,
    pub events: Vec<TranscriptEvent>,
}

pub struct TranscriptEvent {
    pub mutation_id: u32,
    pub payload: TranscriptEventPayload,
}

pub enum TranscriptEventPayload {
    AddMethod { generic_name: String, generic_id_hint: u32, source: String },
    Annotate { target: AnnotationTarget, type_source: String },
}

pub enum TranscriptEventKind {
    AddMethod,
    Annotate,
}

impl SessionArtifacts {
    pub fn to_transcript(&self) -> SessionTranscript;
}

impl SessionTranscript {
    pub fn save(&self, dir: &Path) -> Result<(), TranscriptSaveError>;
    pub fn open(dir: &Path) -> Result<Self, TranscriptLoadError>;
    pub fn restore(
        &self,
        grammar_registry: GrammarFrontendRegistry,
        host_registry: &HostMetaRegistry,
    ) -> ReplayOutcome;
}
```

The on-disk layout is a structured directory:

```text
session.iloko/
  transcript.toml
  base.loko
  mutations/
    000000-add-method.loko
    000002-add-method.loko
    000003-annotate.loko
```

`transcript.toml` is the manifest: `format_version`, `source_file_name`,
`next_mutation_id`, and one `[[events]]` table per mutation carrying
`mutation_id`, `kind`, and a relative `source` path, plus
`generic_name` and `generic_id_hint` for an add-method event or `target`
(the annotation target id) for an annotate event. `base.loko` is the
pristine source text. Each event's source file under `mutations/` is the
full Loko clause an add-method applied, or the type an annotate wrote;
the `NNNNNN-<kind>.loko` filename pattern preserves stable session event
ids and any gaps left by rewind. Annotate events left `format_version` at
1: add-method entries read and write exactly as before, and a build that
predates annotate rejects an annotate event as `UnknownEventKind`. An
event missing its kind's fields, or with a `target` that is not a target
id, fails `open` with `InvalidEventFields`.

**Strict replay only on restore.** An annotate event's `target` resolves
by its stable id against the base, as replay does. `generic_id_hint` is
used verbatim as the request's `GenericFunctionId`. `generic_name` is recorded for
human inspection and future conflict-handling slices, but this slice
does not rebind by name when ids drift between save and open. If
the base source's sem has reordered generic ids since save, restore
surfaces the standard replay failure (`GenericNameMismatch` or
`UnknownGeneric`) rather than guessing which generic the original
mutation meant. Silent rebind would be a different, richer contract.

**Rewind high-water mark is preserved.** The transcript records the
originating session's `next_mutation_id` explicitly. Restore sets the
ephemeral session's counter to that value, not to `max(applied) + 1`
that the apply loop would otherwise compute. This matters when the
originating session was rewound: dropped ids stay retired across save
and open, and subsequent apply calls on the adopted artifacts get
the next never-used id.

**Nothing else is persisted.** The save path writes only the manifest,
the base source, and the event source files. No cached artifacts, no
lowered IR, no tape, no runtime trace, no diagnostic state, no host
registry contents. The live image is derived from the transcript at
restore time; only the transcript is durable.

**Format stability.** `format_version` is the migration bump point.
This build requires exact equality with the build's
[`FORMAT_VERSION`] — both newer and older / 0 / unknown versions are
rejected. Schema changes must be migration-compatible; a slice that
bumps the version must also explain how older transcripts are read.

**Open validates the manifest before touching the filesystem for
events.** A hand-edited or hostile manifest cannot coerce `open`
into:

- reading outside the transcript directory: each event's `source`
  field must equal the canonical
  `mutations/<NNNNNN>-<kind>.loko` path derived from its
  `mutation_id` and `kind`. Absolute paths, `..` components, and
  any non-canonical filename surface
  `TranscriptLoadError::InvalidEventSourcePath`;
- silently re-using ids: events must be strictly increasing in
  `mutation_id`. Duplicates and reordered ids both surface
  `TranscriptLoadError::EventIdsNotStrictlyIncreasing`;
- rolling the id counter backward: `next_mutation_id` must be
  strictly greater than the largest event id. Equality or lower
  would let the next `apply_mutation` on restored artifacts recycle
  an already-assigned id; the check surfaces
  `TranscriptLoadError::NextMutationIdNotAboveMaxEvent`;
- claiming a different schema version: `format_version` must match
  this build's `FORMAT_VERSION` exactly.

These validations live in `open`, before the events are dereferenced
as file paths, so a manifest that would have caused unsafe I/O is
rejected before any I/O happens.

Transcripts are not exposed through `interloko_lab` in this slice. The
lab is stateless per invocation, so a `save` or `open` subcommand
would have no useful state to persist or restore from. A stateful
session subcommand stays deferred along with the rest of the
long-lived session UX.

## Recompute strategy

`RecomputeStrategy::FullCandidateRebuild` is the only variant in this
slice. It names what actually happens: the appended source is recompiled
from scratch and the resulting candidate is atomically swapped in. No
cached query results are reused.

`MutationOutcome::affected_artifacts` reports the dependency-graph truth,
not what was reused from cache. The list contains:

- the front of the pipeline, which re-runs because the source was
  appended to: `Source`, `GrammarParse`, `Parse`, `Ast`, `Bind`;
- everything the artifact graph reaches from the inserted method's
  `DefinitionSem(new)`: its `MethodLookup`, `MethodLowering`, `MethodTape`,
  `Contract`, and `DefinitionTyping`; the affected generic's
  `GenericDispatcher` and `MethodTable`; the `DefinitionTyping` of every
  definition that sends to the generic, since their dispatch verdicts
  assume its method table; and `Typing`, `Diagnostics`, `LoweredModule`,
  `TapeProgram`, `RuntimeResult`.

The drain stops at contracts and method tables whose value the mutation
leaves unchanged (early cutoff), so a generic contract the new method does
not change re-derives without reaching its callers. The list omits
`EnvironmentResolution` and `GrammarStack` (the grammar environment is
unchanged), and omits per-method artifacts for all other methods,
per-generic artifacts for all other generics, and the typing of
definitions that do not send to the generic. For artifacts not in
`affected_artifacts`, post-mutation outputs are byte-equal to pre-mutation
outputs.

CLI and EOP wording must not claim cache reuse. The word "reused" is
reserved for a future `RecomputeStrategy::IncrementalReuse` variant that
will actually deliver it through `IncrementalCompilerSession::apply_mutation`.

## Annotate

`SessionMutation::Annotate { target, type_source }` writes `type_source`
as the type annotation of `target`, a stable binding or result id
(`binding:<definition>:<name>[#<n>]` or `result:<definition>`; see
[`eop_workbench.md`](eop_workbench.md#annotation-targets)). It reuses
add-method's candidate-and-swap flow, checking in order:

1. `type_source` lexes as a type: type names, `class`, `callable`,
   parentheses, commas, and `=>` (`InvalidTypeSource`).
2. The target names a parameter, `let` binding, closure parameter,
   result, or constant value in the current source
   (`UnknownAnnotationTarget`).
3. The edited source compiles without new errors (`ParseFailed`,
   `SemanticFailed`). A type callers or the body violate, or a class
   that does not exist, fails here; new warnings do not.
4. The candidate defines the same classes, generics, functions, methods,
   and constants, and the target reads back with exactly the requested
   type (`InvalidTypeSource`), so text that smuggles in more than a type
   cannot change anything else.
5. The candidate runtime layout validates (`RuntimeValidationFailed`).

The outcome's `MutationEffect::Annotated` carries an `AnnotationOutcome`:
the requested target; `annotated`, the target in the edited source (it
differs only when annotating a method parameter changed the method's
specializers, and so its definition id); the annotation it replaced, if
any; the `SourceEdit` it made; and the `TypingDelta` comparing every
typing decision before and after.
`SessionArtifacts::preview_annotation(target, type_source)` returns the
same `AnnotationOutcome` without swapping, logging, or using a mutation id.

Affected artifacts are computed as for add-method, from the annotated
definition's `DefinitionSem`: a changed parameter or result type changes
the definition's contract, which reaches the typing of its callers; an
annotation that leaves the contract alone (a `let`) stops there.

Policies are not mutations: `set-typing-policy` and `set-codegen-policy`
(and `Session::set_typing_config`, `Session::set_codegen_policy`, the
REPL's `:typing` and `:codegen`) change session inputs that every compile,
rewind, and replay uses, and add no log entry. See
[`eop_workbench.md`](eop_workbench.md#policies) for why.

## Workbench use

```sh
cargo run -p interloko_lab -- invoke-affordance \
  tools/interloko_lab/fixtures/add_method_target.loko \
  generic:g0 \
  add-method \
  --source tools/interloko_lab/fixtures/add_method_square.loko
```

Outcome line:

```text
applied add-method mutation_id=0 inserted_method=m1 affected_artifacts=20 strategy=full-candidate-rebuild log_size=1
```

An annotation reports the same line shape followed by its delta:

```text
applied annotate mutation_id=0 target=binding:def…:s annotation=<circle> previous=<shape> affected_artifacts=14 strategy=full-candidate-rebuild log_size=1
typing delta: changed=2 added=0 removed=0 unchanged=1
...
```

Failure paths print structured rejection messages, for example:

```text
add-method mutation failed: specializer vector duplicates existing method m0
add-method mutation failed: parse failure (3 new diagnostics)
add-method mutation failed: unknown generic id g99
annotate mutation failed: semantic failure (3 new diagnostics)
annotate mutation failed: `<integer> = 3` is not a type annotation
```

## Code under suspended frames

Everything above changes a session between runs. A run can also take new
code while it is suspended: when a condition goes unhandled, the chooser
(the REPL break loop, a scripted chooser) may install a recompiled program
as a new code unit and then retry a frame with it. The runtime keeps every
unit it installed, so each frame finishes the code it entered, while
generic sends from any frame go to the newest unit's methods and a retried
frame calls the newest version of its function or method
(`interloko_runtime::code`).

The suspended frames were compiled against the old definitions, and
compiled code trusts the types its compiler resolved without checking
them: a send to a generic inferred to return `<circle>` reads the result's
slots as a `<circle>`'s, and a function declared to take `s :: <circle>`
reads `s` the same way. Tape signatures erase both to `object`, so each
code unit also carries the type *contract* of every generic, method and
function (`interloko_model::contract::Contract`). A contract comes from
`sem::Module::contract`: declared types, or the value kinds sem inferred
and lowering trusts, such as an open generic's result inferred from its
methods. A method's results are its generic's, since only that generic's
sends and dispatcher call it. Contracts never come from the soft typing
pass or any finer reading of a body, so a body edit changes a contract
only when it changes what callers were compiled to trust.

Newer code may stand in for older code that live frames were compiled
against only when its contract keeps the older one's promise: each
parameter may *widen* (accept at least what callers pass) and each result
may *narrow* (return only what callers expect). Anything else is refused,
as a layout change is:

- **Install.** A unit installed while frames are suspended must keep the
  runtime layout, and every generic must keep its tape signature and keep
  its contract's promise, because old frames send to its newest methods.
  Functions and methods are not checked here: direct calls stay within
  the caller's unit, and methods are reached only through their generic.
  A refused unit is not installed; the chooser is asked again with the
  reason (`UnhandledCondition::refused`), and the REPL reports that the
  edit applies to new runs only.
- **Retry.** A retry passes the frame's own arguments to the newest code
  and returns its results to the frame's old caller. A frame whose
  function or method changed its signature, or broke its contract, is not
  retryable (`FrameCode::retry_refusal`); choosing it anyway is an invalid
  choice. The REPL lists the reason beside the frame and refuses `retry`
  with it, leaving the break waiting.

The check applies only where code compiled against one unit can reach
another's definitions, which is within one run. A run starts from a
single unit and compares nothing with units from earlier runs, so
recompiling with no suspended frames, and running again, accepts any
change.

Lowering under `CodegenPolicy::Proved`, the default, drops runtime checks
that soft typing proved, so proved code trusts typing facts too. Those facts rely on
nothing outside their own body but contracts (a callee's fact is the
category sem gives its result, which its contract records) and the closed
method sets and class cones the runtime layout fixes, so the install and
retry checks above already protect it: a body edit that keeps the contracts
installs under proved frames, and one that breaks a generic's contract is
refused. A constant's value is lowered where it is read, so a fact about it
stays inside the unit that read it. Closure and exit-procedure entries keep
their class checks: typing cannot see who calls them. A break-loop edit is
lowered under the session's current policy, which cannot change while the
break waits; the frames it lists are the same under either policy unless a
send in them was inlined, which leaves no frame of its own.

A check that is kept can fail, and a failed check signals a `<type-error>`
whose `use-value` restart continues the code after it with a replacement
(see [`eop_workbench.md`](eop_workbench.md)). Proved facts survive that
because typing never derives one from a check that may fail beyond what
the check itself establishes: a cast that may fail has its target's type,
not its operand's narrower one, and a conversion or arithmetic result
that may fail has its whole type. The replacement is checked to have that
type (a cast checks a replacement object again), so a fact downstream of
the check holds for it as for the value that passed. A check that must
fail does not make the code after it unreachable for typing, since a
replacement reaches it.

A frame counts as superseded when its definition's newest code differs
in signature or bytecode, so an edit elsewhere should leave unchanged
functions byte-identical. For runtime checks that holds: each function
numbers its own `<type-error>` sites, the condition tags failing checks
resume through and the check-failed tag come first among control tags in a
fixed order, and their request signatures come last among control
signatures. Adding or removing checks, or adding an exit procedure (whose
tag comes after them), renumbers nothing an unchanged function refers to.
Nor do linked sends: a program declares a link for every method of its
runtime layout, and a link's id is its method's id, so adding or removing a
send renumbers no link. What can still renumber it: adding or removing a source function or method
before it (function ids are in source order), and a program's first signal
with a new result type, whose condition tag shifts the exit tags after it.

A send whose method typing selected from its arguments' exact classes is
*linked* under `proved`: it calls the method through the `execution_tape`
link the program declares for that method, instead of asking the runtime
and calling indirectly. Links keep the send late-bound. The runtime layout,
which every unit of a run shares, fixes which method those classes select,
so a link only has to follow its method's newest code: every install
repoints each link a unit's sends call at the new unit's version of the
method, or at the new dispatcher when that version converts its arguments
or results, which is where a dynamic send would go. A linked send relies on
the generic's contract alone, as a dynamic one does, so the install check
above protects it; nothing trusts the selected method's own, narrower
contract. Frames already running a method keep it, and a retry of such a
frame calls the newest version as usual.

Linked sends agree with dynamic sends across runs too. A run selects from
its entry unit and what it installs, so the driver's `Installed` handle
remembers every unit its runs installed, and each run starts by pointing
all their links back at the entry unit's methods; an install during a run
repoints them all at the new code. A value made by a unit an earlier run
installed, such as a returned closure, therefore sends where the same
dynamic send would. The VM always accepts these targets: the units took
over from one another, so they share the layout and each generic's tape
signature, and a link's target has its generic's signature. A chooser sees
every linked send with the function it calls now
(`UnhandledCondition::links`; the break loop's `:links` lists them).

A callable value carries more than its type: code that applies it relies
on a signature, either a declared `callable(<integer>) => <circle>` type
or, for a value sem knows to be a particular function, generic or
accessor, that definition's own signature. Only the known definition's
signature is trusted outright. Nothing checks the callable a value of
declared type holds against the declaration (it may have arrived through
an untyped call), so a call through it relies only on the register kinds
the signature fixes, which the VM checks, and casts each class result.
Within one compile, sem accepts a callable for a declared type by the same
rule a contract uses below. The type lattice's callable atom has no
signature, so a contract records it beside the type, as a nested contract
compared by the same rule. Inside a returned callable the rule applies as
written (it may accept more and return less); inside a callable parameter
it flips, because callers supply that callable and the new code calls it.
A signature that mentions itself, such as a function that returns itself,
is recorded as a reference to the enclosing contract, and a contract
holding such a reference must stay equal. Register kinds inside a
signature (an `<i32>` where an `<integer>` was) are not compared here;
the VM traps a call whose closure has a different signature.

A class object's kind is only a static bound, so a contract records a
class object as any `<class>`. `make` on a class value allocates the class
the value names, checked at run time against the classes the compiled code
can construct, and its result is only known to be in the bound's cone; a
class an edit introduces after the code was compiled traps rather than
being built from a stale plan.

## Deferred

Subsequent slices may add:

- `retract-method`, `edit-body`, `define-class`, `redefine-class`,
  each landing as a new `SessionMutation` variant and a new log entry
  shape rather than a parallel mutation system;
- structural-form method sources (no text input at all);
- branching: forking a session from a prefix of the log into a
  separate session identity (rewind already proves the log can be
  truncated; replay already proves the log can travel; transcripts
  already make sessions portable across processes; branch adds a
  second live session that shares a prefix in the same process);
- explicit conflict-resolution modes for replay and transcript
  restore beyond prefix-only (skip-failed, strict-prefix,
  drop-after-id, etc.) if a real need for them emerges;
- replay / restore with rebind-by-name semantics for cases where the
  new base has reordered generics or classes but kept the names;
- transcript checkpoints / packfiles / binary packing for sessions
  with very long histories where replaying everything on each open
  becomes expensive;
- transcript schema migration tooling once `format_version` is bumped
  for the first time;
- in-language (Loko-from-Loko) mutation calls that append to the same
  log as external CLI-driven mutations;
- a stateful `interloko_lab` subcommand or other long-lived session
  surface that can save / open transcripts, exercise rewind / replay,
  and show multiple log entries from a single CLI invocation;
- mutation affordances on `IncrementalCompilerSession` that genuinely
  skip unrelated work through the query-cache (introducing
  `RecomputeStrategy::IncrementalReuse`);
- a capability policy that decides which callers can request which
  mutations.

These belong to their own plans. They share `SessionMutation`'s
non-exhaustive enum, the candidate-and-swap discipline, the
`MutationOutcome` / `MutationError` / `RewindOutcome` / `RewindError`
/ `ReplayOutcome` / `ReplayFailure` / `TranscriptSaveError` /
`TranscriptLoadError` shapes, and the `AppliedMutation` and
`TranscriptEvent` shapes so the family composes cleanly.
