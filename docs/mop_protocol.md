# Read-Only MOP Protocol

## Overview

Cairn's read-only Metaobject Protocol exposes object-system facts to ordinary
Cairn code. It lets a program inspect classes, slots, generic functions, and
methods through named protocol operations instead of relying only on workbench
dumps.

The protocol is intentionally read-only. Source and semantic analysis still own
class, generic, method, and slot identity. Runtime host calls answer queries
from those facts; they do not create or mutate the object system.

Example:

```dylan
define class <shape> (<object>)
  slot size :: <integer>, init-value: 1;
end;

define class <circle> (<shape>)
  slot radius :: <integer>, init-value: 10;
end;

define function main() => <integer>
  let direct = class-direct-slots(<circle>);
  let slot = vector-ref(direct, 0);
  if class-name(object-class(make(<circle>, radius: 12))) = "<circle>"
  then if slot-name(slot) = "radius" then 1 else 0 end
  else 0
  end
end;
```

## Protocol Operations

Class and object queries:

```text
object-class(value)                 => class metaobject as <object>
class-name(class)                   => <string>
class-direct-superclass(class)      => class metaobject as <object>, or #f
class-direct-slots(class)           => <vector> of slot descriptors
class-effective-slots(class)        => <vector> of slot descriptors
```

Slot descriptor queries:

```text
slot-name(slot)                     => <string>
slot-type(slot)                     => <string>
slot-init-keyword(slot)             => <string>
slot-mutable?(slot)                 => <boolean>
slot-getter(slot)                   => <string>
slot-setter(slot)                   => generic-function metaobject as <object>, or #f
```

Generic and method queries:

```text
generic-function-name(generic)      => <string>
generic-function-methods(generic)   => <vector> of method metaobjects
method-generic-function(method)     => generic-function metaobject as <object>
method-specializers(method)         => <vector> of class metaobjects or #f
```

Vector helpers:

```text
vector-size(vector)                 => <integer>
vector-ref(vector, index)           => <object>
```

## Direct Versus Effective

Direct class facts are source facts. `class-direct-slots(<circle>)` returns the
slots introduced by `<circle>` itself.

Effective class facts are finalized MOP facts. `class-effective-slots(<circle>)`
returns inherited slots followed by direct slots in runtime layout order.

Slot descriptors still name effective runtime storage positions. This keeps
slot reads, writes, object construction, and runtime layout on one simple
representation while still letting the protocol distinguish direct and
effective slot lists.

## Current Limits

- `slot-type` returns declared type text, not a first-class type object. Cairn
  does not yet have a type metaobject layer for `<object>` and future richer
  type expressions.
- `slot-getter` returns the getter accessor name. Getter accessors are not
  generic-function metaobjects yet.
- `slot-setter` returns the compiler-synthesized setter generic when a mutable
  native slot has one. Constant slots and read-only/host-backed slots return
  `#f`.
- The protocol has one visible vocabulary. Legacy first-slice names such as
  `class-slots`, `generic-methods`, and `method-generic` are not accepted.

## Glossary

- **Metaobject:** a runtime value representing an object-system fact such as a
  class, generic function, method, or slot descriptor.
- **Direct slot:** a slot declared directly by a class.
- **Effective slot:** a finalized slot visible on a class after inherited slots
  are merged with direct slots.
- **Protocol operation:** a named read-only operation that asks the object
  system a question.
- **Host call:** the lowered runtime call used to implement a protocol
  operation without adding a special tape opcode.
