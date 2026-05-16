# Interloko EOP Mutation

## Overview

Most EOP affordances are read-only: they project compiler facts as
inspectable records. This slice adds the first *executable* mutation
affordance, `add-method`, which inserts a new method into an existing
generic in a running compiler+runtime session.

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
- mutation affordances other than `add-method`. No
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
}

pub struct MutationOutcome {
    pub mutation_id: u32,
    pub inserted_method: sem::MethodId,
    pub affected_artifacts: Vec<QueryArtifact>,
    pub recompute_strategy: RecomputeStrategy,
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

`apply_mutation` runs these checks in order. Each check produces a
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
arithmetic. The log entry's `outcome.inserted_method` names the sem
method id that the mutation introduced; the candidate-and-swap retag
walks the log on every rebuild and reasserts
`MethodOrigin::Session { sequence: entry.mutation_id }` for each
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
the pristine base and an iterator of `&SessionMutation` requests and
deterministically produces the composed source text. The helper is
deliberately request-only and does not see ids, so the future
replay-against-changed-base slice can supply its own
`AppliedMutation` set without inferring ids from log position.

Rewind reuses the same candidate-and-swap discipline as
`apply_mutation`: a candidate is built and validated; on success the
swap is atomic; on failure the live session is unchanged. The
candidate composition shares the internal
`compose_source_from_log(pristine, requests)` helper, which is also
the seed for the future replay-against-changed-base slice.

Rewind is not exposed through `interloko_lab` in this slice. The lab
is stateless per invocation, so a standalone rewind has no useful CLI
surface; tests in `interloko_compiler` are the verification. A
stateful session subcommand is deferred to a future slice along with
the rest of the "long-lived session" UX.

## Recompute strategy

`RecomputeStrategy::FullCandidateRebuild` is the only variant in this
slice. It names what actually happens: the appended source is recompiled
from scratch and the resulting candidate is atomically swapped in. No
cached query results are reused.

`MutationOutcome::affected_artifacts` reports the dependency-graph truth,
not what was reused from cache. The list contains:

- top-level pipeline artifacts that genuinely change: `Source`,
  `GrammarParse`, `Parse`, `Ast`, `Bind`, `Diagnostics`,
  `LoweredModule`, `TapeProgram`, `RuntimeResult`;
- the inserted method's per-method artifacts:
  `MethodLookup(new)`, `MethodLowering(new)`, `MethodTape(new)`;
- the affected generic's `GenericDispatcher`.

The list omits `EnvironmentResolution` and `GrammarStack` (the grammar
environment is unchanged), and omits per-method artifacts for all other
methods and per-generic artifacts for all other generics. For artifacts
not in `affected_artifacts`, post-mutation outputs are byte-equal to
pre-mutation outputs.

CLI and EOP wording must not claim cache reuse. The word "reused" is
reserved for a future `RecomputeStrategy::IncrementalReuse` variant that
will actually deliver it through `IncrementalCompilerSession::apply_mutation`.

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
applied add-method mutation_id=0 inserted_method=m1 affected_artifacts=13 strategy=full-candidate-rebuild
```

Failure paths print structured rejection messages, for example:

```text
add-method mutation failed: specializer vector duplicates existing method m0
add-method mutation failed: parse failure (3 new diagnostics)
add-method mutation failed: unknown generic id g99
```

## Deferred

Subsequent slices may add:

- `retract-method`, `edit-body`, `define-class`, `redefine-class`,
  each landing as a new `SessionMutation` variant and a new log entry
  shape rather than a parallel mutation system;
- structural-form method sources (no text input at all);
- branching: forking a session from a prefix of the log into a
  separate session identity (rewind already proves the log can be
  truncated; branch adds a second live session that shares a prefix);
- replay against a changed base source file: applying a session's
  mutation log to v2 of the source it was originally compiled against
  (the `compose_source_from_log` helper already accepts an arbitrary
  pristine; the public API stays private until this slice lands);
- log persistence and any on-disk serialization format;
- in-language (Loko-from-Loko) mutation calls that append to the same
  log as external CLI-driven mutations;
- a stateful `interloko_lab mutations <file>` subcommand or other
  long-lived session surface that can show multiple log entries from a
  single CLI invocation (rewind has no useful CLI surface today
  because each lab invocation is stateless);
- mutation affordances on `IncrementalCompilerSession` that genuinely
  skip unrelated work through the query-cache (introducing
  `RecomputeStrategy::IncrementalReuse`);
- session restore, including how to interact with a future persistent
  image without dissolving the in-memory fence;
- a capability policy that decides which callers can request which
  mutations.

These belong to their own plans. They share `SessionMutation`'s
non-exhaustive enum, the candidate-and-swap discipline, the
`MutationOutcome` / `MutationError` / `RewindOutcome` / `RewindError`
shapes, and the `AppliedMutation` log entry shape so the family
composes cleanly.
