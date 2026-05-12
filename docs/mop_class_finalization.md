# AMOP-Informed Class Finalization

This note describes Cairn's current class finalization model. It borrows AMOP
vocabulary because that vocabulary names the right boundaries, but Cairn is not
claiming CLOS compatibility.

## Vocabulary

- A **class metaobject** is the semantic record for a nominal class. Today it is
  `sem::Class`.
- A **direct slot definition** is a slot written in the source class body. Today
  it is `sem::Slot`, stored in `Class::slots`.
- An **effective slot definition** is the computed slot visible on instances of
  a finalized class. Today it is `sem::EffectiveSlot`, stored in
  `Class::effective_slots`.
- An **accessor** is the getter introduced by a direct slot. Its stable identity
  is the original slot definition identity.
- **Finalization** computes superclass facts, effective slots, and deterministic
  runtime layout from source-authored class facts.
- **Runtime layout** is the executable storage view consumed by lowered IR and
  `execution_tape` host calls.

The useful AMOP distinction is not "make Cairn into CLOS." It is "keep the
source class definition separate from the computed class facts used to allocate,
inspect, and optimize instances."

## Source Facts

Cairn currently supports one optional superclass:

```cairn
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

## Construction And Reads

`make(<child>, ...)` validates and fills all effective slots. Inherited init
keywords, required keywords, and `init-value:` defaults are visible during child
construction. Explicit keyword values override defaults.

`child.parent_slot` and `parent_slot(child)` both lower through the effective
slot on the receiver class. The lowered IR and tape path still use ordinary
runtime calls. No inheritance-specific tape opcode exists.

## Dumps And EOP

MOP and semantic dumps show:

- superclass;
- finalization status;
- direct slot count and effective slot count;
- direct slots as source facts;
- effective slots as computed facts;
- direct vs inherited effective-slot source;
- source class/source slot for inherited slots;
- effective layout order.

EOP inspection exposes class and slot records from those same facts. A
`slot:c1.s0` record names the effective slot at layout position `s0` on class
`c1`, while its inspection text points back to the source slot that introduced
it.

## Surface Syntax As Future Metaobject Construction

Dylan-style `define class` syntax can be understood as surface sugar for
constructing class and slot metaobjects, then finalizing them. Cairn does not
execute such metaobject-construction functions yet; the compiler still lowers
syntax directly. The design point is that the eventual reflective protocol has a
place to land.

## Internal Now

- `sem::Class` is the class metaobject record.
- `sem::Slot` is the direct slot definition record.
- `sem::EffectiveSlot` is the computed effective slot record.
- Finalization runs inside semantic lowering.
- Runtime layout is derived from effective slots.
- MOP/EOP/workbench output is a read-only view over these records.

## Deferred

- public AMOP API;
- user-defined metaclasses;
- class redefinition and instance obsolescence;
- multiple inheritance and class precedence lists;
- Dylan inherited slot specifications;
- initialization argument specifications and init methods;
- method combination or inheritance-aware generic dispatch;
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
