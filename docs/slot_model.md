# Cairn Slot Model

This slice treats slots as object-model facts that are visible in semantic IR,
MOP dumps, EOP records, and the workbench.

## Supported Syntax

Untyped slots default to `<object>`:

```cairn
define class <box> (<object>)
  slot value;
end;
```

Typed slots declare a value category:

```cairn
define class <circle> (<object>)
  slot radius :: <integer>;
end;
```

Construction keywords default to the slot name. A slot can rename that keyword
or require it:

```cairn
define class <circle> (<object>)
  slot radius :: <integer>, init-keyword: r:;
  slot diameter :: <integer>, required-init-keyword: diameter:;
end;
```

`init-value:` supplies a default expression when construction omits the slot.
An explicit construction keyword overrides the default:

```cairn
define class <circle> (<object>)
  slot radius :: <integer>, required-init-keyword: r:, init-value: 10;
end;

define function main()
  make(<circle>).radius + make(<circle>, r: 2).radius
end;
```

`constant slot` records mutability metadata. In this slice it means
construction-time assignment only; slot reads behave the same as ordinary
mutable slots because slot writes do not exist yet.

```cairn
define class <box> (<object>)
  constant slot value :: <integer>, init-value: 5;
end;
```

Classes may name one superclass. `<object>` is the root/default superclass:

```cairn
define class <shape> (<object>)
  slot radius :: <integer>, required-init-keyword: radius:;
end;

define class <circle> (<shape>)
end;

define function main()
  make(<circle>, radius: 10).radius
end;
```

A slot typed as a parent class accepts subclass instances:

```cairn
define class <shape> (<object>)
  slot size :: <integer>, init-value: 10;
end;

define class <circle> (<shape>)
end;

define class <holder> (<object>)
  slot item :: <shape>;
end;

define function main()
  make(<holder>, item: make(<circle>)).item.size
end;
```

## Semantics

- Slot type metadata is normalized during semantic lowering. Missing type
  annotations become `<object>`.
- Class finalization separates direct slots from effective slots. Direct slots
  are source-authored facts. Effective slots are computed facts used for
  construction, reads, runtime layout, MOP dumps, and EOP inspection.
- Effective slot order is parent effective slots first, followed by the class's
  direct slots in source order.
- Inherited effective slots keep the original direct slot/accessor definition
  identity while receiving a subclass-local layout slot.
- Init keywords are resolved semantically before lowering. Unknown keywords,
  duplicate keywords, and missing required keywords produce focused
  diagnostics.
- Required keywords may be omitted when the slot has an `init-value:`.
- Explicit construction values and `init-value:` expressions are type-checked
  against the slot type. For nominal object slots, that check accepts the
  declared class or any subclass.
- Slot allocation metadata is currently always `instance`.
- Accessor identity remains the slot definition identity, so stable IDs survive
  harmless declaration trivia edits.
- Host-backed classes remain non-constructible through Cairn `make`.

## Workbench Inspection

Use the workbench to inspect the semantic facts:

```sh
cargo run -p cairn_lab -- dump corpus/pass/objects/constant_slot.cairn --stage mop
cargo run -p cairn_lab -- dump corpus/pass/objects/constant_slot.cairn --stage eop
cargo run -p cairn_lab -- dump corpus/pass/objects/inherited_parent_child_slots.cairn --stage mop
cargo run -p cairn_lab -- dump tools/cairn_lab/fixtures/inherited_dispatch.cairn --stage mop
cargo run -p cairn_lab -- inspect tools/cairn_lab/fixtures/slot_defaults.cairn slot:c0.s0
cargo run -p cairn_lab -- inspect tools/cairn_lab/fixtures/inherited_point.cairn class:c1
```

The slot records show the declared type, init keyword, required/default status,
allocation, mutability, runtime representation, accessor relationship, and
direct/inherited source. For inherited slots, `source-slot=class:cN.sM` points
back to the direct slot that introduced the effective slot. MOP runtime layout
rows repeat the direct/inherited source so it is clear which effective slot
storage came from which direct declaration.

## Deferred Design

This is not the full Dylan initialization protocol. The following are deferred:

- slot writes and setters;
- inherited slot specifications that override inherited defaults;
- class allocation and each-subclass allocation;
- virtual or computed slots;
- before/after/around initialization methods;
- sealed slot optimization;
- persistence/image storage;
- host-backed construction;
- generalized mutability enforcement.
