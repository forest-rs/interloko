# Refinement Typing (LiquidHaskell-Style) for Loko

This note explores what LiquidHaskell-style refinement typing could look like
in Interloko: what LiquidHaskell actually provides, which parts transfer to a
Dylan-shaped, generic-function, gradually-typed language, and a staged path
that starts from machinery the compiler already has.

This is a design exploration, not a milestone commitment.

## What LiquidHaskell Provides

LiquidHaskell layers value-level predicates over ordinary Haskell types:

```haskell
{-@ type Nat = {v:Int | v >= 0} @-}
{-@ divide :: Int -> {v:Int | v /= 0} -> Int @-}
{-@ head :: {xs:[a] | len xs > 0} -> a @-}
```

The pieces that make it work, and that any Loko analogue would need to take a
position on:

1. **Refined base types.** A type is a base type plus a predicate over the
   value (`{v:Int | v >= 0}`). Predicates live in a decidable logic (linear
   arithmetic, equality, uninterpreted functions) so checking stays automatic.
2. **SMT-backed subtyping.** `{v | p}` is a subtype of `{v | q}` when the
   solver proves `p => q` under the current path condition. No manual proofs.
3. **Liquid inference.** Unannotated refinements are inferred as conjunctions
   drawn from a finite qualifier set, so most code needs no annotations.
4. **Measures.** Small total functions (`len`, `size`) lifted into the logic
   so predicates can talk about data structures without full dependent types.
5. **Path sensitivity.** Branch conditions strengthen the environment:
   inside `if x /= 0 then ...`, the solver knows `x /= 0`.
6. **Static-only enforcement.** LiquidHaskell rejects programs at compile
   time; there is no runtime fallback.

Point 6 is where Loko should deliberately diverge. Everything else maps
surprisingly well.

## What Interloko Already Has

The design and implementation already contain the skeleton:

* `DESIGN.md` reserves a semantic type constructor
  `Refinement(base: Type, predicate: PredicateId)` and sketches surface
  syntax:

  ```dylan
  define type <non-empty-sequence>[T]
    = <sequence>[T] where size(value) > 0;
  end;
  ```

* The stated v0/v1 rule is already refinement-first:

  > Value-level constraints are represented as refinements, not generic
  > parameters.

* Semantic lowering already performs class-based occurrence typing:
  `if instance?(x, <circle>)` refines `x` to `object(c1)` in the true branch,
  the refinement is visible in `dump sem`, and it lowers through ordinary
  branches, casts, and traps (see
  `corpus/pass/objects/instance_refinement.loko`). This is exactly the
  path-sensitivity mechanism a predicate refinement needs, currently
  specialized to the single predicate `instance?(x, <class>)`.

* `DESIGN.md` already rules out a `CHECK_REFINEMENT` tape opcode: refinements
  must lower to ordinary comparisons, branches, and traps. That constraint is
  healthy and should stand.

So the question is not "can Interloko represent refinements" but "how much of
the checking becomes static, and how".

## The Loko Shape: Gradual Refinements

LiquidHaskell can be static-only because Haskell is closed-world at
verification time. Loko is an open, interactive, incrementally-compiled
generic-function language. The natural analogue is a **gradual refinement
system** with one governing rule:

> A refinement is a fact the compiler either proves, checks, or refuses —
> and the choice is explicit and inspectable.

For every place a value flows into a refined position, semantic analysis
classifies the obligation:

* **proved** — the solver (or the built-in lattice) discharges it statically;
  no residual code.
* **checked** — a runtime check is inserted at the boundary, lowering to
  ordinary compare/branch/trap, with failure routed through the effect system
  (`<refinement-error>` with restarts, per the existing
  refinement-failure-policy note in the runtime section of `DESIGN.md`).
* **refused** — in a declared-strict region, an unproved obligation is a
  diagnostic instead of a check.

This mirrors how `instance?`/`as` already behave (static knowledge when
available, runtime class test when not) and keeps the tape and runtime
surface unchanged. It also gives a migration story LiquidHaskell lacks:
programs run before they verify, and verification tightens region by region —
which is the same trusted-region idea the staged grammar work already uses.

## Surface Sketch

Named refinement types, per the existing `DESIGN.md` sketch:

```dylan
define type <nat> = <integer> where value >= 0;

define type <non-zero> = <integer> where value ~= 0;

define type <non-empty-sequence>[T]
  = <sequence>[T] where size(value) > 0;
```

Used anywhere a type expression appears — parameters, results, slots:

```dylan
define function divide(n :: <integer>, d :: <non-zero>) => <integer>
  n / d
end;

define class <circle> (<object>)
  slot radius :: <nat>;
end;
```

Inline predicates in signatures can come later; named types first keeps the
grammar small and the diagnostics nameable.

Flow refinement then generalizes the existing `instance?` special case:

```dylan
define function safe-divide(n :: <integer>, d :: <integer>) => <integer>
  if d ~= 0 then divide(n, d) else 0 end
  // true branch: d refined <integer> -> <non-zero>; obligation proved,
  // no residual check.
end;
```

Measures fall out of the object model rather than needing new machinery:
`size`, `radius`, and other slot accessors / sealed generics are already
named semantic entities. A measure is a generic the checker is allowed to
treat as an uninterpreted-but-consistent function in the logic — sealedness
(or a per-generic `measure` adjective) is what licenses that.

## Refinements and Generic Dispatch

This is the genuinely novel design ground — LiquidHaskell has no analogue.
The existing parametric-types rule extends cleanly:

> Method dispatch uses the runtime class/domain specializer. Refinements
> constrain applicability only when statically known or explicitly checked;
> they do not create runtime dispatch domains.

So `define method f(x :: <nat>)` means: specializer `<integer>`, plus a
refinement obligation on entry. Two methods on `<integer>` that differ only
by refinement are a dispatch ambiguity, not a valid overload set. Refinements
refine *contracts*, not *domains*. This keeps method lookup exactly as it is
and avoids solver calls inside dispatch — which the incrementalism and
predictability tenets would forbid anyway.

## Staged Path

Each stage is independently useful and keeps the tape/runtime unchanged.

1. **Checked refinement types (contracts).** Parse `define type ... where`,
   represent `Refinement(base, predicate)` in semantic IR, and insert runtime
   checks at every refined boundary. No solver, no inference. This alone is a
   principled contract system with named, reusable predicates, and it makes
   refinements visible in `dump sem` from day one.
2. **Predicate flow refinement.** Extend the existing `true_branch_refinement`
   machinery from `instance?` to comparison predicates (`=`, `~=`, `<`, `>=`)
   and refined-type membership, tracking per-local predicate sets. Discharge
   obligations with a small built-in lattice (constants, comparisons against
   literals, transitivity-free implication). Many checks from stage 1
   disappear without any SMT dependency.
3. **Solver-backed subtyping.** Generate verification conditions per function
   body and discharge them through a *pluggable* prover interface — plausibly
   modeled as a compiler effect (`perform prove(vc)`), which also makes proof
   results cacheable dependencies for incremental compilation. The solver
   must live behind a replaceable boundary (an external process or optional
   crate), never as a core-compiler dependency: the core stays
   dependency-light, and "no solver installed" degrades to stage-1 checked
   semantics rather than failure.
4. **Liquid-style inference.** Infer refinements for unannotated locals and
   results from a qualifier set harvested from declared refinement types.
   This is the piece that makes LiquidHaskell ergonomic, and the piece to do
   last — it only pays off once stages 1–3 have made obligations and proofs
   observable.

## Risks

- **Solver in the core.** An SMT dependency in `interloko_compiler` would
  violate the dependency-light fence. The prover must be a replaceable
  backend with a checked-semantics fallback.
- **Hidden flow magic.** Predicate refinement multiplies the flow-sensitivity
  already flagged as a risk for `instance?`. The mitigation is the same:
  every refinement and every obligation classification (proved / checked /
  refused) must appear in semantic dumps and fingerprints.
- **Predicate language creep.** The `where` logic must stay decidable and
  small (comparisons, arithmetic, measures). Arbitrary Loko expressions in
  predicates silently turn "types" into "asserts"; if an expression is not in
  the logic, it should be diagnosed as check-only, not absorbed.
- **Dispatch entanglement.** Any design where applicability depends on solver
  results makes dispatch non-predictable and non-incremental. Refinements
  must stay contract-level.
- **Mutation.** LiquidHaskell leans on purity; Loko has mutable slots. Flow
  refinements on slot reads are only sound between potential writes. Stage 2
  should refine locals only; refining slot contents can wait for
  effect-informed purity information.
