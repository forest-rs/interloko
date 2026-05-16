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

- `retract-method`, `edit-body`, `define-class`, `redefine-class`;
- structural-form method sources (no text input at all);
- mutation affordances on `IncrementalCompilerSession` that genuinely
  skip unrelated work through the query-cache (introducing
  `RecomputeStrategy::IncrementalReuse`);
- session restore, including how to interact with a future persistent
  image without dissolving the in-memory fence;
- a capability policy that decides which callers can request which
  mutations.

These belong to their own plans. They share `SessionMutation`'s
non-exhaustive enum, the candidate-and-swap discipline, and the
`MutationOutcome`/`MutationError` shape so the family composes cleanly.
