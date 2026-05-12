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

## Semantics

- Slot type metadata is normalized during semantic lowering. Missing type
  annotations become `<object>`.
- Init keywords are resolved semantically before lowering. Unknown keywords,
  duplicate keywords, and missing required keywords produce focused
  diagnostics.
- Required keywords may be omitted when the slot has an `init-value:`.
- Explicit construction values and `init-value:` expressions are type-checked
  against the slot type.
- Slot allocation metadata is currently always `instance`.
- Accessor identity remains the slot definition identity, so stable IDs survive
  harmless declaration trivia edits.
- Host-backed classes remain non-constructible through Cairn `make`.

## Workbench Inspection

Use the workbench to inspect the semantic facts:

```sh
cargo run -p cairn_lab -- dump corpus/pass/objects/constant_slot.cairn --stage mop
cargo run -p cairn_lab -- dump corpus/pass/objects/constant_slot.cairn --stage eop
cargo run -p cairn_lab -- inspect tools/cairn_lab/fixtures/slot_defaults.cairn slot:c0.s0
```

The slot records show the declared type, init keyword, required/default status,
allocation, mutability, runtime representation, and accessor relationship.

## Deferred Design

This is not the full Dylan initialization protocol. The following are deferred:

- slot writes and setters;
- inherited slots;
- class allocation and each-subclass allocation;
- virtual or computed slots;
- before/after/around initialization methods;
- sealed slot optimization;
- persistence/image storage;
- host-backed construction;
- generalized mutability enforcement.
