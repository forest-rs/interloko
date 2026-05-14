# AMOP-Informed Class Finalization

This note describes Interloko's current class finalization model. It borrows AMOP
vocabulary because that vocabulary names the right boundaries, but Interloko is not
claiming CLOS compatibility.

## Vocabulary

- A **class metaobject** is the semantic record for a nominal class. Today it is
  `sem::Class`.
- A **direct slot definition** is a slot written in the source class body. Today
  it is `sem::Slot`, stored in `Class::direct_slots`.
- An **effective slot definition** is the computed slot visible on instances of
  a finalized class. Today it is `sem::EffectiveSlot`, stored in
  `Class::effective_slots`.
- An **initialization plan** is the computed class fact that orders effective
  slots for construction and records accepted init keywords, requiredness,
  defaults, and explicit-argument override behavior. Today it is
  `sem::InitializationPlan`, stored in `Class::initialization_plan`.
- An **init argument** is a semantic call-site fact recorded for an explicit
  `make(<class>, key: value)` keyword argument.
- An **accessor** is the getter introduced by a direct slot. Its stable identity
  is the original slot definition identity.
- **Finalization** computes superclass facts, effective slots, and deterministic
  initialization/runtime layout facts from source-authored class facts.
- **Nominal subtyping** follows the single superclass chain. A subclass instance
  is acceptable where its parent class is expected.
- **Runtime layout** is the executable storage view consumed by lowered IR and
  `execution_tape` host calls.

The useful AMOP distinction is not "make Interloko into CLOS." It is "keep the
source class definition separate from the computed class facts used to allocate,
inspect, and optimize instances."

## Source Facts

Interloko currently supports one optional superclass:

```loko
define class <shape> (<object>)
  slot x :: <integer>, required-init-keyword: x:;
end;

define class <point> (<shape>)
  slot y :: <integer>, required-init-keyword: y:;
end;
```

`<object>` is the root/default superclass. Omitting the superclass list is the
same as naming `<object>`.

The source facts are:

- class name and stable definition identity;
- explicit superclass name, if written;
- direct slot declarations in source order;
- direct slot type, init keyword, required/default metadata, allocation,
  mutability, origin, and accessor identity.

## Finalization Facts

Semantic lowering resolves the superclass, then finalizes each class. Effective
slot order is deterministic:

1. parent effective slots, in parent effective order;
2. direct slots from the class, in source order.

Inherited effective slots preserve the original direct slot/accessor definition
identity while receiving a subclass-local layout slot. That means a child can
read an inherited slot with the right runtime index without pretending the slot
was redefined in the child.

Finalization diagnoses:

- unknown superclasses;
- primitive superclasses such as `<integer>` and `<boolean>`;
- host-backed classes used as superclasses;
- duplicate effective slot names;
- duplicate effective init keywords;
- duplicate effective accessor names.

This slice intentionally rejects inherited slot conflicts instead of
implementing Dylan-style inherited slot specifications or AMOP-style slot
option combination.

After effective slots are finalized, Interloko computes an initialization plan for
the class. The plan is not another source of slot truth; it is the construction
view of the effective slots. Plan steps appear in the same inherited-then-direct
layout order as effective slots. Each step records the accepted keyword, source
slot identity, declared kind, whether the keyword is required, whether the slot
has an `init-value:` default, and that explicit init args override defaults.

## Construction And Reads

`make(<child>, ...)` validates and fills all effective slots. Inherited init
keywords, required keywords, and `init-value:` defaults are visible during child
construction. Explicit keyword values override defaults.

Semantic IR now records the explicit init arguments at the `make` call site and
the planned field initializers chosen from the class initialization plan.
Diagnostics distinguish unknown keywords, duplicate keywords, missing required
keywords, explicit initializer type mismatches, and default `init-value:` type
mismatches.

Lowered IR consumes the initialization plan when emitting object construction.
It does not iterate effective slots ad hoc, and it does not introduce a new
execution_tape opcode. The tape still sees ordinary runtime object construction
and host calls.

`child.parent_slot` and `parent_slot(child)` both lower through the effective
slot on the receiver class. The lowered IR and tape path still use ordinary
runtime calls. No inheritance-specific tape opcode exists.

## Nominal Subtyping And Dispatch

Single inheritance is now a semantic subtype relation:

- `instance?(make(<child>), <parent>)` is true;
- `as(<parent>, make(<child>))` is accepted as an upcast;
- `<parent>`-typed slots can hold child instances;
- direct function parameters and returns accept child values where a parent is
  expected;
- generic parameter contracts accept subclass method specializers and subclass
  call arguments;
- restart handlers specialized on a parent condition class can receive subclass
  condition instances.

Generic dispatch is still single-argument nominal dispatch. The lowered
dispatcher checks runtime class IDs and chooses the most-specific applicable
method in the superclass chain. This is ordinary lowered control flow plus
ordinary calls; execution_tape still does not know about generic dispatch or
inheritance as special opcodes.

Sibling classes remain unrelated. `instance?` and `as` between unrelated
classes still produce impossible-test/cast diagnostics when the static classes
are disjoint.

## Dumps And EOP

MOP and semantic dumps show:

- superclass;
- full superclass chain;
- finalization status;
- direct slot count and effective slot count;
- direct slots as source facts;
- effective slots as computed facts;
- direct vs inherited effective-slot source;
- source class/source slot for inherited slots;
- effective layout order;
- initialization plans, accepted keywords, required/default status, slot order,
  and explicit-argument override behavior.
- runtime layout rows with superclass and slot source;
- dispatch applicability for each method, including subclass classes that can
  select a parent-specialized method.

EOP inspection exposes class and slot records from those same facts. A
`slot:c1.s0` record names the effective slot at layout position `s0` on class
`c1`, while its inspection text points back to the source slot that introduced
it. Generic and method records include the dispatch applicability summary used
by the lowered dispatcher.

Dispatch sites are exposed as compiler-generated EOP objects with IDs such as
`dispatch:d0`. A dispatch-site explanation names the enclosing function or
method, source span, generic contract, static argument kind, applicable methods
in specificity order, rejected methods and reasons, the generated dispatcher
function, and the related method lookup/dispatcher/lowered/tape artifact keys.
This is still an explanation of the current single-argument nominal dispatcher,
not a new method-combination or optimization protocol.

Useful workbench commands:

```sh
cargo run -p interloko_lab -- dump tools/interloko_lab/fixtures/inherited_point.loko --stage mop
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/inherited_point.loko class:c1
cargo run -p interloko_lab -- dump tools/interloko_lab/fixtures/inherited_dispatch.loko --stage mop
cargo run -p interloko_lab -- dispatch tools/interloko_lab/fixtures/inherited_dispatch.loko
cargo run -p interloko_lab -- dispatch tools/interloko_lab/fixtures/inherited_dispatch.loko dispatch:d0
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/inherited_dispatch.loko generic:g0
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/inherited_dispatch.loko dispatch:d0
cargo run -p interloko_lab -- diff tools/interloko_lab/fixtures/inherited_point_parent_before.loko tools/interloko_lab/fixtures/inherited_point_parent_after.loko
```

## Surface Syntax As Future Metaobject Construction

Dylan-style `define class` syntax can be understood as surface sugar for
constructing class and slot metaobjects, then finalizing them. Interloko does not
execute such metaobject-construction functions yet; the compiler still lowers
syntax directly. The design point is that the eventual reflective protocol has a
place to land.

## Initialization As Future Protocol

This slice is intentionally shaped toward Dylan's `make`/`initialize` model, but
it does not execute user-defined `initialize` methods yet. Today, slot
initialization is the whole protocol: validate the init args, choose explicit
values or defaults, and construct the object in initialization-plan order.

The substrate added here is meant to be reused by a later protocol. A future
`make` can allocate an instance, pass the same init-argument records to
`initialize`, and let user-defined methods consume or forward them. Before that
is enabled, Interloko still needs decisions about `make` as a generic function,
custom allocation, keyword forwarding, `next-method`, method combination,
reinitialization, class redefinition, and mutation/setter interaction.

## Internal Now

- `sem::Class` is the class metaobject record.
- `sem::Slot` is the direct slot definition record.
- `sem::EffectiveSlot` is the computed effective slot record.
- `sem::InitializationPlan` is the computed construction view of effective
  slots.
- `sem::InitArg` is the semantic record for one explicit `make` keyword
  argument.
- `session::DispatchSiteRecord` is the compiler/session record for one semantic
  generic call site.
- Finalization runs inside semantic lowering.
- Runtime layout is derived from effective slots.
- MOP/EOP/workbench output is a read-only view over these records.

## Deferred

- public AMOP API;
- user-defined metaclasses;
- class redefinition and instance obsolescence;
- multiple inheritance and class precedence lists;
- Dylan inherited slot specifications;
- user-defined `initialize`, `make` genericization, init-arg forwarding, and
  full initialization protocol hooks;
- method combination;
- slot setters/writers and mutation enforcement;
- sealed slot optimization;
- persistence/image storage;
- host-backed construction.

## References

- The Art of the Metaobject Protocol, Kiczales/des Rivieres/Bobrow, MIT Press,
  1991.
- CLOS MOP class finalization and `compute-slots` descriptions:
  <https://www.metamodular.com/CLOS-MOP/class-finalization-protocol.html> and
  <https://lisp-docs.github.io/cl-language-reference/meta-object-protocol/dictionary/compute-slots>.
- Dylan slot options and inherited slot specifications:
  <https://jim.studt.net/dirm/interim-31.html> and
  <https://package.opendylan.org/dylan-programming-book/slots.html>.
