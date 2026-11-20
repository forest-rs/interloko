# Interloko

Interloko is an experimental compiler/runtime substrate for a small
Dylan-shaped source language called Loko.

The short version:

- **Interloko** is the compiler, runtime, object model, and inspection
  substrate.
- **Loko** is the source notation, stored in `.loko` files.
- **Vivarium** is the possible future collaborative environment built on top of
  this substrate.

This repository is not trying to be a production language yet. It is a working
place to make object-system semantics, generic dispatch, host interop,
incremental compilation, and inspectable execution concrete.

## Try It

Run one corpus program:

```sh
cargo run -p interloko_lab -- run corpus/pass/generics/two_class_dispatch.loko
```

Generate the staged-grammar report demo:

```sh
cargo run -p interloko_lab -- report tools/interloko_lab/fixtures/staged_records.loko --output /tmp/staged-records.html
```

Open `/tmp/staged-records.html` directly in a browser. That report is the
current best demo of the grammar/frontend work: a trusted `records` region
emits structural class forms, the AST consumes those forms directly, and the
report links source spans to generated classes, slots, accessors, artifacts,
lowered IR, tape, and runtime trace output.

Inspect a compiler stage:

```sh
cargo run -p interloko_lab -- dump corpus/pass/generics/two_class_dispatch.loko --stage sem
cargo run -p interloko_lab -- dump corpus/pass/generics/two_class_dispatch.loko --stage tape
```

Run the small-program corpus:

```sh
cargo run -p interloko_corpus -- check corpus
```

Start the REPL:

```sh
cargo run -p interloko_repl
```

Run the float calculator example, either over its built-in formulas or on an
expression in the staged calculator grammar:

```sh
cargo run -p interloko_float_calculator_example
cargo run -p interloko_float_calculator_example -- "(1.0 + sqrt(5.0)) / 2.0"
```

## A Small Loko Program

```loko
define class <circle> (<object>)
  slot radius :: <integer>;
end;

define class <square> (<object>)
  slot side :: <integer>;
end;

define generic area(x);

define method area(c :: <circle>)
  c.radius * c.radius
end;

define method area(s :: <square>)
  s.side * s.side
end;

define function main()
  area(make(<square>, side: 9))
end;
```

That program parses through the normal pipeline:

```text
source -> syntax -> AST -> semantic IR -> lowered IR -> execution_tape
```

Generic dispatch is represented in Interloko's semantic/lowered IR, then
emitted to `execution_tape` through ordinary calls and runtime host calls rather
than special generic-dispatch bytecode.

## What Works Today

The current slices include:

- function definitions, locals, arithmetic, booleans, conditionals, and
  calls; as in Dylan, a parameter without a type is an `<object>`, so
  `define function add(a, b) a + b end` adds whatever numbers it is passed
  through the numeric generics; as in Dylan, results may be named,
  `=> (sum :: <integer>)`, a bare name, `=> (value)`, is an `<object>`,
  and the names only document (`#rest` results are refused: a callee
  returns a fixed number of values); a function, method or closure
  declared `=> ()` returns no values, and as in Dylan its body's last
  expression runs for its effects with its values discarded;
- Dylan's conditionals: `if (test) … elseif (test) … else … end`,
  `unless (test) … end`, and `case test => …; otherwise => … end`. Each arm
  is a body, with its own `let`s, whose last expression is its value. A
  conditional without `else` or `otherwise` produces no values: its arms
  run for their effects, and using it where a value is expected is a type
  error. An `instance?` test narrows its arm, and each later `elseif`,
  `else` or `case` arm sees what the earlier tests ruled out. As in Dylan,
  a conditional's value has the union of its arms' types, so any arms join:
  arms of different classes at their least common superclass (a `<call>`
  arm and a `<var>` arm at `<node>`), callables of different signatures at
  `<function>`, and anything else at `<object>`, boxing a scalar arm on its
  way out, so `if (digit?(c)) digit-value(c) else #f end` is the "integer
  or false" of dynamic code. Soft typing still sees the union of the arms'
  types (`<integer> ∪ #f` there). The same holds for `select`, `handle` and
  a `block` with an exit.
  Where its values are discarded, as in a statement, at the end of a
  body declared `=> ()`, or at the end of a loop's body or a `block`'s
  cleanup, a conditional's arms, or a `handle`'s body and restart, run for
  their effects and need not agree;
- bodies of `let`s and `expression;` statements, where, as in Dylan, `;`
  separates expressions, so a body's last expression is its value with or
  without one. A body with no expression to end it, empty (`while (#f)
  end`, `method () => () end`, `begin end`, an empty arm) or ending in a
  `let`, produces no values, as an `if` without `else` does;
- assignable `let` locals and parameters: `x := v` is an expression whose
  value is the new value. A local keeps the type it declares, or its
  initializer's when it declares none, so `let n = 0; n := "s"` is an
  error, and a limited local checks each value assigned to it. As in
  Dylan, a parameter is assignable too, and assigning it changes only the
  parameter: an assigned parameter moves out of the register the call
  passed it in, so retrying a frame after an edit passes the arguments it
  was called with. Block exits, `for` loop variables, a restart's
  condition and constants are not assignable;
- Dylan's anonymous functions, `method (params) => results body end`,
  which take the parameter and result annotations a `define function` does.
  One written where a typed callable is expected (an argument for a
  `callable(...)` parameter or keyword, a typed
  callable slot's value or default, a `let`, constant or assignment
  declared `callable(...)`, or a result declared one) takes each parameter
  type it does not write, and its results when it writes none, from that
  signature, so `fold(xs, method (a, b) a + b end)` adds integers inline
  when `fold` takes a `callable(<integer>, <integer>) => <integer>`. Its
  results keep the first value of a body that returns more, so
  `method (a, b) truncate/(a, b) end` fits `=> <integer>`. A type it writes
  stays, and is an error if it does not fit; a method of another arity
  takes nothing. Its entry checks what it takes as it would a written
  type, and typing cites the signature where it relies on one
  (`expected(c…)` in its reasons). Only a method written directly in such
  a position takes a signature, not one in an `if` arm or `begin` there,
  and a `define generic`'s parameters carry no callable signatures, so a
  method passed in a send takes none;
- Dylan's local methods: `local method name (params) => results body end,
  method other (…) … end;` binds each method to its name for the rest of
  the body, as an anonymous method closing over the body's locals. Every
  name of a group is in scope in each of its methods, so they recurse, by
  themselves or together, and the group's bindings then live in shared
  cells made before the methods are. Within its group a method that writes
  its result types is called through them, and one that writes none as an
  untyped `<function>`, its results known only once its body is; after the
  group each is called as itself;
- closures that share assigned locals: a `let` local or parameter that is
  assigned and that a closure refers to lives in a heap cell its frame and
  every capturing closure use, so counters, accumulators and callbacks that
  update state work, and the cell lives as long as a closure keeps it,
  within the run. Only such locals pay for the cell (a host call per read
  or write); every other local stays in a register and every other capture
  is a copy. Since any call may run a closure that stores in the cell,
  typing reads a shared local at its declared type;
- `while (test) body end` and `until (test) body end` loops on a
  `<boolean>` test, which produce no values and run in one frame as tape
  jumps, so a loop of a million iterations needs no call depth. Soft typing
  analyzes a loop from a head whose facts are the fixpoint over every
  iteration, so its decisions (and a strict definition's proofs) hold on
  each one, and an assignment ends any narrowing of the local it assigns.
  A session call may run 100 million tape instructions;
- `select (target) key, key => …; otherwise => … end`, and `select (target
  by test) …` to match with `test(target, key)` instead of `==`. The target
  is evaluated once, and a `by` test once after it. Integer keys against an
  integer target compare inline, as do character keys against a
  `<character>`, and boolean keys against a `<boolean>` without boxing;
  matching with `==` never dispatches. An arm whose keys are all integer
  literals, all character ones or all boolean ones, sees a local target as
  one of them. Without `otherwise`, a target that matches no key signals a
  `<select-error>` holding it, so unlike `if` and `case` the arms keep
  their values;
- numeric `for` loops: `for (i from a below b) body end`, with `to b`
  (through `b`, counting down when the step is negative) or `above b`
  instead of `below`, an optional `by step` (1 by default), and no bound
  at all for a loop something else exits. `a`, `b` and the step are
  `<integer>`s, each evaluated once before the first iteration. The loop
  lowers to a `while` over a counter the body cannot name, so typing and
  code generation treat it as that loop, and it produces no values. Each
  iteration binds `i` afresh as a `let` would, `i :: type` included, but
  `:=` cannot assign it, so a closure may always capture it (that
  iteration's value). The counter steps with checked arithmetic after each iteration, so
  a loop through the largest `<integer>` signals `type-error.overflow`
  after its last iteration;
- collection `for` loops: `for (x in c) body end` binds `x` to each
  element of `c` in turn, an `<object>`; `x :: type` checks each with
  `as`. Several clauses, `for (x in a, i from 0, y in b)`, step together
  and end when the first runs out. A vector visits its elements in order,
  reading its size each step, so a stretchy vector's elements added during
  the loop are visited; a table visits its values in the order their keys
  were added, and adding or removing a key while it is iterated signals a
  `<collection-modified-error>` when the loop moves on (replacing a value
  does not). Over a `<vector>` or `<table>` typing knows, the loop reads
  the collection directly, as the equivalent `while` would; over any other
  collection it makes one `forward-iteration-protocol` send at entry and
  calls the callables it answers. That is Dylan's protocol without
  `copy-state`, since every state is an immutable value: an initial state,
  a limit, `next-state(c, s)`, `finished-state?(c, s, limit)`,
  `current-key(c, s)`, `current-element(c, s)`, and
  `current-element-setter(v, c, s)` or `#f` for a constant collection. It
  is an open built-in generic, sealed over `<vector>`, `<string>`, `<table>`
  and `<sequence>` itself. A string's states are its `<string-position>`s,
  so the key of each character indexes the string, and it has no element
  setter. Over a `<string>` typing knows, `for` steps positions itself and
  reads each character and the next position with one tape decode, binding
  a `<character>`, about 50 ns per character; under an annotation a
  character may not satisfy, it is cast as an `<object>` element would be,
  and typing warns that the cast always fails, which strict typing refuses.
  Over a string typing does not know, it goes through the protocol's
  callables, one boxed character per step. The abstract `<sequence>` class, whose
  subclasses are `<vector>`, `<string>` and a program's own, has a default
  method iterating from 0 below
  `size(c)` with `element`, so a program's sequence class iterates once it
  defines `size` and `element`; any other class defines its own method;
- Dylan-style names, where a hyphen is always a name character: `x-1` and
  `a-b` are single names, so subtraction is spaced (`x - 1`, `a -% b`),
  `?` and `!` may end one (`digit?`, `add!`), and a `/` that does not
  start a `//` comment is one too (`truncate/`), so division is spaced
  (`x / 2.0`);
- `<integer>` arithmetic on 64-bit fixnums that signals a `<type-error>`
  (`type-error.overflow`) instead of wrapping when `+`, `-`, `*`, or
  negation leaves the 64-bit range (bignum promotion is planned);
  Dylan's integer divisions `truncate/(a, b)`, `floor/`, `ceiling/` and
  `round/` (a tie to the even quotient) answer the quotient and the
  remainder `a - q * b` as two values, a zero divisor signals a
  `<division-by-zero-error>` whose `use-value` supplies them, and the
  least integer divided by -1 signals `type-error.overflow`; `floor` and
  the other roundings leave an integer as it is;
- declarations checked where values meet them, as in Dylan: a value whose
  type is a supertype of a binding's, argument's, slot's, constant's or
  result's declared type, an `<object>` read from a table most often, is
  checked there as `as` would check it, unboxed on the way to a scalar,
  and signals a `<type-error>` (`type-error.cast`) with `use-value` when it
  fails; an argument is checked as it is evaluated, before the next one.
  A callable signature is more than a class, so a binding that declares
  one still takes only a value known to have it. Soft typing records each check and proves it away when it can; a
  value of a class it can never be an instance of is still an error. Where
  one value is expected, a call returning several keeps the first (an
  argument, a `let` without a values pattern, a `select` arm); one
  returning none is an error. A call through an untyped `<callable>` keeps
  the values it wants of those the value returns in the same way, none at
  the end of a `=> ()` body: the value's shape records how many it returns,
  and the call tests that count against each larger one a callable of the
  program returns (a value from code installed later that returns a count
  the program never saw is called as the call site expects, which the tape
  checks);
- immutable top-level constants named in the Dylan style, such as
  `define constant $answer :: <integer> = 42;`. The initializer may be any
  expression, including a `begin … end` body with its own `let`s, loops and
  closures, and may call functions and send generics. A constant is
  evaluated once, at its first read in a run, and every later read gets that
  value, so `make` in an initializer makes one object; a number, boolean,
  symbol or metaobject (and arithmetic over them) folds instead, read in
  place like a literal. Constants whose initializers read one another in a
  cycle are an error; a read the compiler cannot see, through a function
  call, signals a `<constant-cycle-error>` naming the constant and the
  definition that read it (`constant.read-during-initialization`). Each run (each session call) initializes its
  constants again, and code a hot swap installs initializes its own while
  older frames keep theirs;
- `begin body end`, a body as an expression, whose `let`s are its own and
  whose last expression is its value;
- statements as operands, as in Dylan: an `if`, `unless`, `case`,
  `select`, loop, `begin`, `block` or `handle` may appear wherever any
  other operand may, so `x + if (c) 1 else 2 end`, `f(select (k) … end)`
  and `begin … end[0]` work. One that produces no values, such as an `if`
  without `else`, has no value to give an operator or argument;
- nominal classes with typed slots, init keywords and inherited slots, and
  Dylan's slot defaults: `slot x = expr` evaluates `expr` at each `make`
  that takes it, so `slot bindings :: <table> = make(<table>)` gives each
  instance its own table, and may be any expression, a `begin … end` body
  with its own `let`s, loops and closures included; `init-function: f`
  calls `f` at each such `make`; and `init-value: expr` is evaluated once,
  every instance sharing its value (once per run, as a constant is: a
  literal, name or anonymous method stays in place, and any other value
  is kept in a hidden constant). A literal default is computed in place.
  As in Dylan, a slot takes one default and one construction keyword, and
  `required-init-keyword:` takes no default;
- slot getters and setters that are generic functions, as in Dylan: every
  class declaring a slot `power` gives the generic `power` a method reading
  it (and `power-setter` one writing it), so `x.power` of an `<object>`
  holding any of them dispatches on its class, through a guarded link that
  caches the method the class selects, and the getter is a value like any
  generic. A receiver of a class with the slot reads or writes it directly:
  a getter is sealed over the classes declaring its slot, as Dylan's
  `sealed slot` is, so a method for one of them or a subclass is refused
  (`sem.method-overrides-slot-getter`), while a method for another class
  joins it. A program's own generic of the name takes the getters as
  methods if it takes one receiver and admits the slot's class and what
  it holds, and a function or constant of the name leaves the getter to
  receivers of the slot's classes. An `<integer>` slot named `size` joins
  the kernel's `size`; a slot named for another collection generic, such
  as `element`, serves its own classes only, and is sealed the same way.
  A slot named like a built-in, such as `apply` or `make`, serves a call
  of one receiver of its class, and every other call of the name is the
  built-in's;
- generic functions and multi-argument method dispatch over nominal classes;
  as in Dylan, a method parameter that writes no type is specialized on
  `<object>`, so `define method label(thing, extra)` is a fallback taking
  any arguments;
- Dylan's keyword parameters: `define function scale (x, #key by :: <integer>
  = 2, plus = 0)`, called as `scale(3)`, `scale(3, by: 10)` or `scale(3,
  plus: 1, by: 5)`, keywords after the positional arguments in any order.
  A call evaluates its positional and keyword arguments in source order,
  then the default of each keyword it does not pass in declaration order;
  a default may read the required parameters and the keywords declared
  before it. A keyword without a type is an `<object>`, and one without a
  default is `$unsupplied` when a call does not pass it: one kernel object
  (a direct `<object>` instance) that no other value is `==`, so passing
  any value, the symbol `#unsupplied` included, counts as passing it, and
  the name is reserved, so no definition can rebind it; any other type
  needs a default. Each default is a
  hidden function, `scale #key by`, held to strict typing when its owner
  is, that callers which do not pass the keyword call, so a typed call is a direct call with every keyword in
  place, never a parse at run time, and editing a default changes only that
  function, while editing a keyword's type changes its callers' contract.
  Unlike Dylan, where each method has its own defaults, a generic declares
  its keywords, their types and their defaults (`define generic area (s,
  #key scale :: <integer> = 1)`), a send fills the defaults from the
  generic, a method binds any of them under the generic's types and gives
  none of its own, and dispatch looks only at the required parameters.
  A function or generic that takes keywords is a value like any other:
  a call through a value of known signature passes every keyword, boxed
  and in declaration order, with a kernel marker source cannot write for
  those it leaves out (so passing `$unsupplied` explicitly means what it
  means in a direct call), and the value's entry fills their defaults and
  checks their types; a call through an untyped `<callable>` passes the
  keywords it names by name, to a value whose shape says it takes
  keywords, and a value that does not take one signals
  `<unknown-keyword-error>`, which holds the keyword, rather than dropping
  it. Calls passing no keywords, and values taking none, keep the plain
  boxed callable ABI, so they cost nothing more. A typed callable type may
  declare keywords too, as in
  `callable(<integer>, #key by :: <integer>) => <integer>`, and only a
  value with exactly those keywords has it. `apply` passes no keywords.
  Statically, a keyword its callee does not take, or one passed twice, is
  an error. Anonymous functions cannot declare keywords yet;
- Dylan's `#rest` parameters: `define function sum (first, #rest more)` or
  `method (#rest xs) … end` binds the arguments after the required ones to
  a `<vector>` the call makes, constant as a literal is, since Dylan's
  rest sequence is the callee's to read. A direct call, a call through a
  binding of the function and a typed call through a value make the
  vector at the call. A call through an untyped value passes a value
  taking `#rest` its arguments' count and every argument in one vector,
  which the value's shape says it takes, and the value's boxed entry
  splits off the required ones, signalling `<argument-count-error>` for
  too few as a call of the wrong arity does; a call through an untyped value tests the shape only
  when the program has such a value. A `#rest` parameter takes no type,
  being always a `<vector>`, generics and methods do not take one yet,
  nor does a parameter list with `#key` (so a keyword argument through an
  untyped value taking `#rest` signals `<unknown-keyword-error>` rather
  than landing in the vector as in Dylan), and whether a definition takes
  one is part of its contract: a value taking `#rest` never stands in for
  one taking a `<vector>` parameter, nor the other way round;
- Dylan's `apply(f, a, …, sequence)` calls `f` with `a, …` and then each
  element of `sequence`, which may be any `<sequence>`: a vector, a
  string, or a program's own sequence class through the iteration
  protocol. `f` is evaluated first, then the arguments in order, which
  are collected into one vector (the sequence itself when it is a
  `<vector>` and nothing precedes it), and the call goes through `f`'s
  value under the boxed callable ABI, its entry checking each element as
  it checks an untyped call's argument. A value taking `#rest` gets the
  vector whole; any other gets as many arguments as the vector holds, a
  number lowering checks against the value's when it knows its signature
  and otherwise against each number a callable value of the program takes,
  and a vector of the wrong size signals `<argument-count-error>`. Each element is a positional
  argument: `apply` passes no keywords;
- Dylan's class and generic adjectives: `define abstract class` (no direct
  instances, so `make` refuses it), `define sealed class` and `define
  sealed generic`, with `open` and `concrete` as the explicit defaults.
  Classes and generics are open unless sealed: Interloko is edited live,
  and an open class is one a later edit may extend without disturbing code
  compiled against it. There is one compilation unit, so sealing confines
  a class's direct subclasses and a generic's methods to the defining
  source: a session `add-method` on a sealed generic is refused, and so is
  code installed into a running program that breaks what its code relies
  on. In return soft typing reads a cone over a sealed class as the union
  of its pieces (its own instances unless it is abstract, and each direct
  subclass's cone): a dispatch over it is covered when each piece has a
  method, casts and `instance?` tests are decided piece by piece, an
  `instance?` else branch keeps only the other pieces, and a sealed class
  with no subclasses is exact, so its sends link. Every such proof records
  the closed-world assumption it relies on, such as `sealed abstract class
  <shape> has exactly direct subclasses {<circle>, <square>}`;
- opt-in strict typing, the checked end of the progression from loose to
  sealed code: `define strict function`, `method` or `constant`, and
  `define strict generic` for every method of a generic, or the `strict`
  typing policy for every definition at once. A strict definition must
  prove the checks the compiler inserts into it: every send's dispatch,
  every range check where a value meets a `limited` declaration, and every
  cast where a value meets a narrower declared type, one returned through
  an untyped `<callable>` included. Each one typing
  leaves to the runtime is an error (`typing.unproved-dispatch`, `-range`,
  `-cast`) naming its decision, the reasons behind the operand types, and
  the fixes (annotate a parameter, test with `instance?`, seal a class, or
  write an `as`), and so is any check that always fails. An explicit
  `as(<circle>, s)` is how strict code checks deliberately, `instance?`
  never fails, and overflow stays a runtime check unless the policy says
  `strict.overflow=proved`, since `<integer>` is held in 64 bits for now.
  Strictness only changes reporting: a strict definition compiles exactly
  as it otherwise would, and once its errors are fixed its checks are
  proved and dropped. Its errors block running and installing the code,
  as every compile error does. A preview reports what would fail before opting in.
  Strict is not check-free: explicit `as`, `instance?`, indirect-entry
  guards and (by default) overflow stay runtime checks, and annotating a
  parameter proves a body's check by moving it to the callers, a contract
  change every annotation preview names. Each definition's claim report
  (`claims:<definition>`) says what typing established, what the generated
  code relies on, and why each remaining check stays;
  see [`docs/eop_workbench.md`](docs/eop_workbench.md#strict-typing),
  and `interloko_corpus strictness` measures how much of the corpus
  already meets it;
- a numeric class tower: `<number>` > `<real>`, then `<rational>` >
  `<integer>` and `<float>` > `<double-float>`, where only `<integer>` and
  `<double-float>` are concrete; methods may specialize on any of them, and
  an abstract specializer such as `<real>` or `<float>` receives the boxed
  value unless the generic fixes a scalar parameter type, while annotations
  name a concrete class;
- booleans: `#t` and `#f` are the instances of the concrete `<boolean>`
  class, so methods may specialize on it, `instance?` and `class-of` see it,
  and a boolean boxes to `<object>` and back like a number;
- `a & b` and `a | b`, Dylan's short-circuit operators: they share one
  precedence below the comparisons and associate left, the right operand
  runs only when it decides the answer, and it sees what the left one
  established, as an `if` arm does (`instance?(x, <box>) & x.n > 3`);
- Dylan's negations: `~test` is `if (test) #f else #t end` on a
  `<boolean>`, binding tighter than any binary operator as unary `-`
  does, and `a ~= b` and `a ~== b` are `~(a = b)` and `~(a == b)`, at the
  comparisons' precedence;
- `base ^ exponent`, Dylan's exponentiation, right-associative and binding
  tighter than `*` but looser than a unary operator, so `-2 ^ 2` is `4` and
  `2 ^ 3 ^ 2` is `512`. An integer power is exact and computes as
  `<integer>`, overflow-checked as `*` is (`type-error.overflow`); a
  negative integer exponent signals `type-error.range` expecting
  `limited(<integer>, min: 0)`, and `use-value` supplies the power either
  way. With a float operand it computes `pow` in the widest float present.
  Typed operands compute inline, and typing proves both checks for a
  bounded base and a bounded exponent that cannot be negative; other
  operands send the sealed built-in `^` generic, as `+` does;
- characters: `<character>` is a concrete class of Unicode scalar values,
  written `'a'`, `'\n'`, `'\t'`, `'\r'`, `'\0'`, `'\\'`, `'\''` or
  `'\u{1F600}'` (string literals take the same escapes). A character is an
  unboxed scalar, never an `<integer>`: `=`, `<` and the other comparisons
  and `==` compare scalar values inline, `as(<integer>, c)` gives the
  value, and `as(<character>, i)` checks that `i` is a scalar value (in
  range and not a surrogate), signalling `type-error.range` with
  `use-value` otherwise. Methods may specialize on `<character>`, and a
  character boxes to `<object>` and back without allocating. The
  predicates `digit?` and `digit-value` are ASCII only; `alphabetic?`,
  `alphanumeric?`, `whitespace?`, `uppercase?` and `lowercase?` use
  Unicode properties, with ASCII-only `ascii?`, `ascii-alphabetic?`,
  `ascii-alphanumeric?`, `ascii-whitespace?`, `ascii-uppercase?` and
  `ascii-lowercase?`; `as-uppercase` and `as-lowercase` map one scalar to
  one, leaving a character whose mapping is longer (`ß`) unchanged.
  `digit-value` of anything but an ASCII digit signals `type-error.range`
  with `use-value`. User definitions may shadow these names;
- callables as objects: functions, closures and generic functions used as
  values are instances of the concrete `<function>` class, and every
  callable type, `<callable>` or `callable(...) => ...` with a signature,
  is a subtype of it. A callable boxes to `<object>`, so it can live in
  `<object>` slots and vectors and be passed to `<object>` parameters;
  `class-of`, `instance?` and dispatch see `<function>`, and methods and
  generic parameters may specialize on it. `as(<function>, o)` casts one
  back out as an untyped callable, whose calls check their arguments and
  results like any untyped call (`type-error.entry` and `use-value`).
  A callable is `==` itself boxed or not, and two values of the same
  function with nothing captured, such as two mentions of one function,
  are `==`. Like a callable in a slot, a boxed one holds its environment
  only for the invocation that made it. As in Dylan, the innermost
  binding of a name wins: a local, parameter or captured binding shadows
  any definition of the program of that name, and a definition of the
  program (a function, constant, generic or slot getter) shadows any
  built-in of the kernel's, `make`, `as`, `instance?`, `signal`,
  `limited`, `apply`, `values`, `class-of`, `concatenate` and the float
  and text functions included. A program's methods join a built-in
  collection generic instead, as Dylan's do. Code the compiler writes
  reaches the kernel by names no source can spell, so `c[k]` always calls
  the kernel's `element`, and the kernel's `<sequence>` iteration its
  `size` and `element`, whatever the program defines.
  A binding is called by its name: `let f = scale; f(3, by: 2)` calls
  `scale` directly, keywords and defaults included, since the binding
  always holds it, while a `<function>`, `<callable>` or typed callable
  binding is called through its value, typed when its signature is known.
  Calling a binding whose value is not callable is an error. As in
  Dylan, any operand can be called, not just a name: `f(1)(2)`,
  `node.operator.apply(x)` and `table[k](x)` evaluate the operand, then
  the arguments, and call its value, typed when its signature is known.
  An `<object>` operand is checked to be a `<function>` as a declaration
  would check it (`type-error.cast`), as it is evaluated and so before the
  arguments, and called through its boxed entry,
  and an operand whose type can never be a function is an error.
  Built-in functions are values as well: one with a single overload, such
  as `digit?`, `floor/` or `class-name`, is a hidden kernel function made
  once per program, so `digit? == digit?`, and one with several, such as
  `sqrt` (a `<double-float>` or a `<single-float>`), `floor` or
  `as-uppercase` (a `<character>` or a `<string>`), is a built-in generic
  whose kernel methods are its overloads. That generic is sealed over its
  methods' classes as `size` is, so a program's `define method sqrt` joins
  it for a class of its own, never for a kernel class
  (`sem.built-in-method-on-kernel-class`), and a call no overload takes as typing sees
  it, an `<object>` or such a class among its arguments, sends to it.
  `shift-left` and `shift-right` as values shift a `<u32>`. As in Dylan,
  `\+` names the function an operator calls: `\+`, `\-` (subtraction;
  unary minus is `negative`), `\*`, `\/`, `\^`, `\=`, `\<`, `\<=`, `\>`
  and `\>=` are the built-in generics those operators send to when their
  operands are not numbers typing knows, and `\==`, `\~==` and `\~=` are
  kernel functions of two objects, so `select (x by \<)` and `fold(\+,
  0, v)` work. A typed call
  of a built-in's name, or of a binding that holds one, still runs it
  inline. `apply`, `values`, `make`, `as`, `limited`, `signal`,
  `copy-sequence` and `starts-with?` are forms whose arguments or results
  no callable type describes yet, and naming one as a value is an error;
- identity: `a == b` is Dylan's `==`, defined on any two objects, never
  dispatching and not user-definable. `=` is Dylan's equality, defined on
  any two objects too: numbers compare by value, characters by scalar
  value, strings by their text, types as types, and anything else by `==`
  (collections included, for now: Dylan compares them element by element),
  so `a = b` of two `<object>`s sends to the `=` generic, whose kernel
  methods those are, and never signals. Heap objects are `==` when they
  are the same object (two strings with the same text need not be).
  Numbers, booleans and characters have no identity apart from their
  value, so they are `==` when they are the same class and value: integers by value whatever limited type holds
  them (`1 == 1i32`), floats by class and IEEE 754 bit pattern, so a NaN is
  `==` itself, `0.0 == -0.0` is false and `1 == 1.0` and
  `1.5f32 == 1.5` are false. Symbols, classes and other metaobjects are
  `==` when they name the same thing. Boxing a value as `<object>` never
  changes the answer;
- floats: literals such as `1.5`/`2e10` are `<double-float>` (IEEE 754
  binary64) instances under the abstract `<number>` > `<real>` > `<float>`
  classes, with slots, boxing to `<object>`, `<double-float>`
  annotations and method dispatch, and IEEE `+ - * /`,
  negation, and comparisons; an integer meeting a float is converted to a
  float for arithmetic and compared exactly (`2^53 + 1 > 2.0^53`), by the
  rules in `promotion.rs`, and `/` on two integers is rejected. When an
  operand is `<object>`, abstract numeric or a user class, the operator
  dispatches through a built-in generic (`+`, `<`, `negative`, ...) whose
  methods come from the same rules; these generics show up in EOP and soft
  typing like any other and are sealed over `<real>`; explicit
  `as(<double-float>, n)` and exact
  `as(<integer>, x)` conversions, which convert a number held as an
  `<object>` by its class at run time, as Dylan's `as` does (between
  numbers only: a boxed `<character>` is cast, and the checks the compiler
  inserts at declarations only cast), and a float library (`abs`, `min`, `max`,
  `remainder`, `nan?`, `finite?`, `floor`, `ceiling`, `round`, `truncate`,
  `sqrt`, `cbrt`, `exp`, `log`, `log2`, `log10`, trigonometric and
  hyperbolic functions, `atan2`, `pow`, `hypot`) whose names user
  definitions may shadow; `float-to-string`, `integer-to-string`, and
  `concatenate` build result text;
- 32-bit numbers for graphics, colour and audio code: `<single-float>`
  (IEEE 754 binary32, a second concrete `<float>`) and the limited integer
  types `<i32>` and `<u32>`, which are `limited(<integer>, ...)` ranges
  rather than classes, so their values are `<integer>`s that widen
  implicitly and box as integers. Literals take a suffix (`-7i32`,
  `255u32`, `0.5f32`), and an unsuffixed literal adopts the 32-bit type its
  context expects when the value fits (`let level :: <u32> = 255`). They
  live in slots, cross `<object>` (a `<single-float>` keeps its format), and
  travel as `FunctionValue::I32`/`U32`/`SingleFloat` through typed calls and
  host functions. Arithmetic follows `promotion.rs`: one limited type stays
  in it, checked (`type-error.overflow` when a result leaves the
  range), mixed integer types compute as `<integer>`, floats are contagious
  to the widest format (`single + double` is a double, `integer + single`
  a single), and comparisons are exact across all of them. Statically typed
  operands compile to the `i32`/`u32`/`f32` instructions directly;
  `<object>` operands dispatch through the same numeric generics.
  `as(<u32>, x)` and friends convert exactly between every numeric type
  (out-of-range integers and non-integral floats signal
  `type-error.range`), and
  `instance?(x, <u32>)` tests for an `<integer>` in the range.
  As values, `<i32>` and `<u32>` are `<limited-integer>` type objects. Wrapping is
  only ever explicit: `+%`, `-%` and `*%` compute modulo 2^32 on two
  operands of one fixed-width type. `logand`, `logior` and `logxor` are
  exact on every integer type, and `shift-left`/`shift-right` shift an
  `<i32>` or `<u32>` by 0 to 31 bits (other counts are rejected or
  signal).
  The float library takes single-floats too, inline in binary32 where the
  tape has an instruction and through the binary64 library (rounded once
  back) otherwise;
- `limited(<integer>, min: 0, max: 255)` types, with either bound optional,
  wherever a type goes (parameters, results, `let`, slots, constants,
  generics, callable signatures, `as` and `instance?`). The values stay
  `<integer>`s; each declaration checks what reaches it (the caller checks
  an argument, where typing usually proves it), so the code reading it may
  assume the range, and the ranges are part of a definition's contract.
  Written as a value, `limited(...)` is a `<limited-integer>` type object,
  as in Dylan, which `instance?` tests against and `=` compares by range.
  An `<integer>` reaches a limited type with that implicit check because
  the representation does not change, while reaching `<i32>` or `<u32>`
  changes it and so takes an explicit `as`. A method's limited parameter
  restates or widens its generic's range: unlike Dylan's DRM, methods do
  not dispatch on ranges, so a send's range check is the only one;
- symbols and keywords; a symbol's name is any word after `#`, reserved
  words included, so a lexer may mark its last token `#end`, and Dylan's
  `#"…"` names a symbol with any text (`#"two words"`). `as(<symbol>, s)`
  interns a string's text, so it answers the literal's symbol, and
  `as(<string>, s)` gives a symbol's name; a boxed value converts by its
  class at run time, as a number does. Unlike Dylan's, symbols keep their
  case: `#Foo` and `#foo` are different symbols;
- strings: as in Dylan, adjacent string literals are one literal, so
  `"ab" "cd"` is `"abcd"` and long text may be written across lines, with
  comments between its pieces. A `<string>` is immutable, validated UTF-8 whose header caches
  its character count, so `size(s)` is O(1), and whether it is all ASCII.
  A string literal is a constant, as in Dylan: every evaluation of a
  literal with the same text during a run is the same string (`==`);
  a Rust host borrows its text as `&str` without a copy. Strings have no
  integer keys. A `<string-position>` is an opaque, unboxed place between
  characters (a byte offset with its own class, never an `<integer>`)
  that only the string functions make: `start-position(s)`,
  `end-position(s)`, `next-position(s, p)`, `previous-position(s, p)`,
  `position-at(s, n)` (O(1) for ASCII text, O(n) otherwise, as its name
  warns) and `position-after(s, prefix, p)`. `s[p]` and `element(s, p,
  default: c)` read the character at `p`, and as in Dylan the default
  may be any object, `element(s, p, default: #f)` answering `#f` at or
  past the end and otherwise the character as an `<object>`, `copy-sequence(s, start: p,
  end: q)` copies, `starts-with?(s, prefix, at: p)` tests without
  allocating, positions compare with `=` and `<`, and `byte-offset(p)`
  and `as(<string-position>, s, n)` cross to byte offsets. A position is
  checked where it is used, in bounds and on a character boundary, so one
  from another string signals `type-error.range` with `use-value` rather
  than splitting a character. `=` compares contents and `<` orders by
  character; `as-uppercase` and `as-lowercase` map a whole string fully
  (`"ß"` to `"SS"`); `concatenate` takes characters too (and, as
  Dylan's does, any number of sequences of any kind: `concatenate(#(1),
  #[2], #(3))` is a list, of the first one's kind; strings typing knows
  join inline, and a program's own `concatenate` methods serve calls of
  two arguments that may be of their classes), and `as(<string>,
  c)` makes one. `string-to-integer(s)`, common Dylan's, reads the
  optional sign and decimal digits `s` starts with, up to the first
  character that is not a digit, and signals a `<simple-error>` when it
  starts with none (or with more digits than an `<integer>` holds);
  `string-to-integer(s, default: d)` answers `d` instead, as an `<object>`
  unless `d` is an `<integer>` (`base:`, `start:` and `end:` and the
  second value, where reading stopped, are still to come). A `<string-builder>` (`make(<string-builder>)`, `add!(b,
  c)` or `add!(b, s)`, `size(b)`) is the one mutable text type, and
  `as(<string>, b)` takes its text without a copy, leaving it empty; a
  builder in the image cannot change during a turn yet. `s[p]`,
  `element`, `next-position`, `previous-position`, `start-position` and
  `end-position` read the string's UTF-8 in tape code, with no host call
  unless a position has no character there; the other operations are
  runtime host calls. A loop reading a character and stepping costs
  about 60 ns per character: `s[p]` followed by `next-position(s, p)`
  decodes once when nothing between them can change `s` or `p`. Normalization and grapheme clusters are
  left to a library;
- mutable collections, which live in the runtime's heap as instances do
  (a turn's stores into image collections are logged, undone if it aborts
  and evacuated if it finishes). Their elements and keys are `<object>`s,
  so numbers and callables box on the way in and come back out as
  `<object>`s:
  - `<vector>`: `make(<vector>, size: n, fill: x)` makes `n` elements of
    `x` (`size:` 0 and `fill:` `#f` by default; at most 2^28 until the
    heap has a byte budget), and `v[i] := x` replaces one. A vector literal `#[a, b]` is constant, as Dylan's literals are,
    and so are the vectors the MOP answers: storing into one signals an
    `<immutable-collection-error>`. An index outside a vector, read or
    written, signals a `type-error.range` whose expected type is the
    vector's indices, `limited(<integer>, min: 0, max: size - 1)`, and whose
    `use-value` replacement is another index, checked again;
  - `<stretchy-vector>`, a `<vector>` that grows at its end: `add!(v, x)`
    appends `x` and returns `v`, and `make(<stretchy-vector>)` takes
    `size:` and `fill:` too;
  - `<table>`, a map from keys to values made with `make(<table>)`: `t[k]`,
    `t[k] := v`, `element(t, k, default: d)`, `size(t)`, `key-exists?(t,
    k)` and `remove-key!(t, k)`, which answers whether the key was there.
    Keys compare as Dylan's `<table>` compares them, by `==`, except
    strings, which compare by their text as `=` does (as Dylan's
    `<string-table>`): `t["max"]` finds the entry stored under any other
    `"max"`, while `1` and `1.0`, or two vectors with the same elements,
    are different keys, as are a character and the integer of its code. A
    callable key, like a callable in a slot, holds
    only for the run that made it. A missing key without a `default:` signals a
    `<not-found-error>` holding the table and the key, whose `use-value`
    supplies the element. Entries keep the order they were added in, and
    a version counter counts changes to the set of keys, which iteration
    checks;
- Dylan's lists: `<list>` is a sealed abstract `<sequence>` whose two
  subclasses are `<pair>` and `<empty-list>`. `pair(head, tail)` makes a
  pair of any two objects, `#()` is the empty list, the one instance of
  `<empty-list>` (so `l == #()` tests for the end), and `list(a, b)` and
  the literal `#(a, b)` make a list of pairs ending in it, evaluating
  their elements in order (`list` is a value too, taking `#rest`) (a literal's pairs are made afresh each time,
  and unlike a vector literal's they are not constant: storing into one
  is not refused). `head(l)` and `tail(l)` take a list
  apart, and as in Dylan both answer `#()` for `#()`; `head-setter(v, p)`
  and `tail-setter(v, p)` store into a `<pair>`. An `<object>` argument is
  checked to be a `<list>` (or a `<pair>`) as a declaration would check
  it. As in Dylan, `x.f` is `f(x)` and `x.f := v` is `f-setter(v, x)` for
  any `f` no class has a slot of, so `p.head`, `l.size` and `p.tail := l`
  work; where a program's class has a slot `head` too, `.head` of a value
  typing knows cannot be of that class still calls the built-in, and a
  call `head(l)` is always the built-in's, but `.head` of an `<object>`
  reads the slot's getter, which serves only the program's classes. As
  a sequence a list has `size` (its number of pairs), `l[i]`, `element(l,
  i, default: d)` and `l[i] := v`, an index past its end without a
  default signalling an `<error>`, and `for (x in l)` walks it from its
  first pair to its last, through its `forward-iteration-protocol`, whose
  states are its pairs. These are the kernel's methods for `<list>`, over
  which the generics are sealed, written in Loko in the kernel library
  (below). A pair is a runtime object of two slots, which `head` and
  `tail` read on the tape, without a host call;
- Dylan's collection generics `size(collection)`, `element(collection,
  key, #key default)`, `element-setter(value, collection, key)` and
  `add!(collection, element)` (whose kernel methods take a stretchy vector
  and any element, or a string builder and a character or string), and its
  element syntax: `c[k]` is `element(c, k)` and `c[k] := v` is
  `element-setter(v, c, k)`, so they nest and chain with slot reads
  (`lexer.text[lexer.pos]`). They are open built-in generics, as the
  numeric operators are built-in generics: the kernel's methods come from
  `BUILTIN_METHODS` in `sem/collection.rs` (`<vector>`'s, and so
  `<stretchy-vector>`'s, `<table>`'s, `<string>`'s `size` and `element` by
  position, and `<string-builder>`'s `size`), a program adds methods for its own
  classes with `define method`, a send typing resolves to a kernel method
  runs it inline, an `<object>` receiver dispatches, and a send no method
  accepts signals `<no-applicable-method>`. Each generic is sealed over the
  kernel classes it has methods for, so a program's method for `<vector>`
  is refused. A program's own function or generic of one of these names
  replaces the built-in generic for calls of the name, though `c[k]` and
  the kernel's iteration still reach the kernel's, and a slot named `size`
  keeps its getter for its own class. `default` is a real keyword parameter of `element`,
  so `element(c, k, default: d)` returns `d` for a key outside the
  collection whatever `c` is, and a program's method takes it as `#key
  default`, testing `default == $unsupplied` when the call passed none. A
  typed `c[k]` runs the kernel's plain operation inline, without testing
  for a default;
- the kernel library: the parts of the kernel written in Loko
  (`crates/interloko_compiler/src/sem/library.loko`): the list methods
  of the collection generics, and Dylan's collection functions over any
  collection, through the iteration protocol: `map(f, c)`, `reduce(f,
  initial, c)`, `reverse(c)`, `reverse!(c)` (which reuses a list's pairs
  and copies anything else), `empty?(c)`, `member?(value, c)` (by `==`,
  Dylan's default test) and `first`, `second` and `third` (by position in
  a string). One that makes a collection makes one of its argument's
  kind, as Dylan's `type-for-copy` has it: a list from a list, a string
  from a string, a stretchy vector from a stretchy vector and a vector
  from anything else, a table included for now. They are functions, not
  generics a program's method can join, and take no keywords yet (no
  `test:` or `default:`); each is a value as a built-in is. A definition
  of the library joins a program only when the program needs it, a
  function when a call or a value names it, a method of a collection
  generic when the program uses the generic, and is the kernel's code: it sees none of
  the program's definitions, so a program's own `size` or `tail` cannot
  change what it does, strict typing does not hold it, and its trees carry
  no source positions. The library is parsed once per compile that needs
  it;
- recoverable condition/restart forms: `handle body with restart e :: <c>
  when guard => body end`, whose body and restart are bodies with their
  own `let`s, and Dylan's `block () body cleanup body exception (e :: <c>)
  body end`, a restart around the block's body that runs after its
  cleanup, with the block's exit procedure in scope, which takes the
  values the block's context expects, or one `<object>` where it expects
  nothing narrower, as an arm the body's values join (one exception clause
  per block for now, and no `test:` or `init-arguments:` options); with
  `<condition>`, `<error>`,
  `<type-error>`, `<no-applicable-method>`, `<select-error>`,
  `<not-found-error>`, `<immutable-collection-error>`,
  `<collection-modified-error>`, `<constant-cycle-error>`,
  `<unknown-keyword-error>`, `<simple-error>`, `<argument-count-error>` and
  `<stack-overflow-error>` defined by
  the kernel. Dylan's `error(condition)` signals a condition as `signal`
  does, and `error(format-string, argument, …)` signals a `<simple-error>`
  (an `<object>` first argument is told apart at run time)
  whose `condition-format-string` is the string and whose
  `condition-format-arguments` is a vector of the arguments; either way,
  like `signal`, it takes the values its context expects, since it does
  not return unless a handler supplies them (`error`, which takes any
  number of arguments, is a form, not a value, for now). In the kernel: a runtime check that fails (a cast, a callable value's argument or result,
  a range, an overflow) signals a `<type-error>` naming the value, the
  expected type (a type object: a class, a `<limited-integer>` or a
  `<subclass-type>`, which `instance?` tests values against), the check's
  typing decision and a stable `type-error.*` code, and its `use-value`
  restart continues with a replacement of the expected type (an object a
  failed cast checks again); a send that finds no method signals a
  `<no-applicable-method>` naming the generic and the arguments
  (`dispatch.no-applicable-method`), whose `use-value` supplies the send's
  results, checked against the generic's declared result types; and a call
  through a callable value passing more or fewer arguments than the value
  takes signals `<argument-count-error>`, holding both counts
  (`argument-count-error-expected` and `-actual`), instead of trapping:
  the value's shape records how many it takes, and the call tests that
  before it crosses;
- read-only MOP inspection for classes, slots, generics, methods, and dispatch;
- embedding-declared scalar host functions with typed integer, float,
  32-bit, boolean, and string arguments/results, EOP discovery, and a handler-aware call surface;
- read-only grammar MOP inspection plus trusted staged grammar adapters;
- structured EOP records for environment/workbench objects and affordances;
- explicit host-backed class/slot metadata for Rust interop experiments;
- soft typing that classifies every runtime check, and lowering that, under
  the default `proved` codegen policy, drops or specializes the checks it
  proved (a numeric send typing resolved runs its method inline);
- compiler sessions, incremental artifacts, cache reports, and stage dumps;
- structured diagnostics rendered through MessageFormat 2 catalogs.

## Retained Sessions

For embedding in a retained tool, [`interloko_session`](crates/interloko_session/README.md)
provides accepted compiler sessions with revision-checked source replacement,
object inspection, typed scalar calls, explicit host handlers, mutation and
rewind. Rejected edits preserve accepted code and history. The wrapper is
`no_std + alloc`; callers own transport, persistence and scheduling.

## What This Is Not Yet

Interloko is intentionally not:

- a stable public language or Dylan compatibility project;
- a persistent image or database;
- the Vivarium product/environment layer;
- an LSP, watch mode, or IDE;
- a macro system;
- a Cranelift backend;
- a full module system (sealing confines extensions to the one source a
  compile reads);
- a sealed-dispatch optimizer beyond typing's proofs: a send over several
  sealed pieces with different methods still asks the runtime.

The project is useful because the compiler and runtime already expose enough
structure to inspect what the object model means and how it lowers, without
pretending the larger environment exists yet.

## Repository Layout

```text
crates/interloko_model       shared ids and small substrate facts
crates/interloko_runtime     runtime layout, host calls, object storage
crates/interloko_compiler    Loko parser, AST, semantic IR, lowering, sessions
crates/interloko_session     retained, revision-checked embedding sessions

tools/interloko_lab          command-line workbench for dumps and inspection
tools/interloko_corpus       corpus runner for small .loko programs
tools/interloko_diagnostics  diagnostic catalog loading/rendering
tools/interloko_repl         small interactive REPL over compiler sessions

corpus/                      pass/fail .loko fixture programs
examples/                    Rust embedding examples
docs/                        focused design notes
```

Useful docs:

- [`DESIGN.md`](DESIGN.md) explains the broader compiler/runtime direction.
- [`docs/interloko_boundary.md`](docs/interloko_boundary.md) explains the
  Interloko/Loko/Vivarium naming split.
- [`docs/mop_protocol.md`](docs/mop_protocol.md) describes read-only MOP
  protocols.
- [`docs/grammar_mop.md`](docs/grammar_mop.md) describes the read-only grammar
  projection, grammar registry, and staged adapter boundary.
- [`docs/eop_workbench.md`](docs/eop_workbench.md) describes environment object
  records and workbench affordances.
- [`docs/eop_mutation.md`](docs/eop_mutation.md) describes the in-memory
  live-mutation contract for the executable `add-method` EOP affordance.
- [`docs/interloko_report.md`](docs/interloko_report.md) explains the static
  HTML report command and its staged-grammar demo path.
- [`corpus/README.md`](corpus/README.md) explains corpus expectations.

## Workbench Commands

`TranscriptLoadError::Manifest` owns its TOML deserializer cause in a `Box` so
transcript-loading errors stay compact. Code constructing that variant should
use `Box::new(error)`; matching and inspecting the cause through a reference
continues to work through dereferencing.

`interloko_lab` is the quickest way to see what is going on:

```sh
cargo run -p interloko_lab -- run <file.loko>
cargo run -p interloko_lab -- dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|typing|types|all
cargo run -p interloko_lab -- artifacts <file.loko>
cargo run -p interloko_lab -- report <file.loko> --output <report.html>
cargo run -p interloko_lab -- dispatch <file.loko> [dispatch-site-id]
cargo run -p interloko_lab -- inspect <file.loko> <object-id>
cargo run -p interloko_lab -- affordances <file.loko> <object-id>
cargo run -p interloko_lab -- diff <before.loko> <after.loko>
```

Every command takes `--codegen off|proved`, anywhere in its arguments, to
choose whether lowering acts on what soft typing proved (the default,
`proved`) or keeps every runtime check (`off`, for debugging the unoptimized
machinery); the REPL's `:codegen [off|proved]` shows or sets the same policy.
Results and runtime outcome codes are the same under both; traces show fewer
sends under `proved`, and a failed check in inlined numeric code signals
from the call. See [Codegen Policy](docs/eop_workbench.md#codegen-policy).

Diagnostics can be rendered with the bundled catalogs or a custom catalog:

```sh
INTERLOKO_DIAGNOSTIC_LOCALE=pseudo cargo run -p interloko_lab -- run corpus/fail/diagnostics/unresolved_local.loko
INTERLOKO_DIAGNOSTIC_CATALOG=path/to/catalog.mf2.toml cargo run -p interloko_lab -- run some-file.loko
```

## Development

The normal validation set is:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc --workspace --all-features --no-deps
cargo run -p interloko_corpus -- check corpus
```

Core crates should stay small and boring. New examples, experiments, and
benchmarks should live in top-level workspace crates rather than adding
development dependencies to the core crates.

## Execution Tape Contract

The compiler pins `execution_tape` to a git revision of `forest-rs/execution`
`main` (see the root `Cargo.toml`) until the next crates.io release. Lowering records
each register's storage kind when assigning its identity; the backend seeds an
assembler from the declared function ABI and appends backend temporaries.
It does not recover declarations by scanning emitted instructions. Closure
environments and indirect signatures use the VM's compositional contracts.

Compiled tapes are regeneratable artifacts. The current wire version is 0.6;
recompile older tapes and regenerate any source sidecars with them. Runtime
results use an explicit owned output vector and remain readable after the run
that produced them ends.

Boxed `<object>` values are VM objects of one host type. Floats (both
formats), booleans, characters, integers within 47 bits and references to runtime
objects travel inside the object handle and never allocate a box; wider
integers live in the runtime host's heap.
`instance?`, `class-of`, and a cast to a class whose subclasses the
layout lists in a few classes read an object's class with `obj.key` on the
tape, without a host call; only a cast that fails asks the host, which
signals. A `make` of a class literal reads no class metaobject.
A slot of a native object (an instance of a program's or the kernel's
classes) that holds an inline value, anything but a callable, is read
and stored with `obj.field` and `obj.set_field`: the runtime answers
them directly, and a store into the image is logged as a `slot_set`
host call's is. `==` compares boxed handles with `obj.eq`, which
decides it alone when either operand is such an object; otherwise equal
handles are identical and the host decides the rest. A run keeps a
constant's value, a string literal's string and the kernel markers in
computation globals once it has them (`global.get`), so only the first
read asks the host. `interloko_lab trace` lists the slot reads and
stores as object queries.

The runtime heap has two regions. Each call is one *turn*: everything it
allocates (instances, strings, vectors, metaobjects, heap boxes) goes to a
nursery that is emptied in one step when the call returns or traps. The
long-lived image holds interned symbols and objects an embedding creates
outside a turn. `SessionArtifacts::runtime` returns a `SessionRuntime`
whose image persists across calls (the REPL uses one; `:heap` shows it),
while `SessionArtifacts::call_function` uses a fresh one per call.
Turns are transactional: a turn's stores into image objects are recorded
in a write log, and a trapped turn replays it backwards. Abort restores
everything except symbols interned during the turn, which stay interned:
interning is idempotent, so keeping them is harmless, and it means the
image never reuses an index, so a symbol handle captured during an aborted
turn keeps naming the same symbol. When a turn returns, the nursery entries
reachable from its roots (values the caller keeps, image roots registered with
`InterlokoHost::register_root`, and the image slots its write log names)
are evacuated into the image in increasing nursery order, charged in work
units against an evacuation budget; everything else is dropped.
Nursery handles are numbered across turns rather than per turn, so a raw
handle an embedder keeps past its turn is stale: resolving it fails with
`RuntimeError::StaleNurseryHandle` instead of naming a later turn's
object. The numbering wraps only after 2^44 nursery objects (2^45 heap
boxes); image handles never go stale.
`FunctionCallOutcome::turn` reports what a call allocated (the nursery
high-water mark), what was reachable at the end, what was promoted, the
symbols it interned, the evacuation work and the number of logged stores;
`interloko_corpus heap [root...]` prints survival for every program with a
`main`.

Each call runs under `RunLimits`: fuel, of which every VM instruction costs
one unit and every runtime host call `HOST_CALL_FUEL` (8) more, so the VM
keeps no separate count of host calls; and a call depth, 100,000 frames by
default, enough for recursion tens of thousands of calls deep. The VM's
contract, aggregate and byte work budgets get as many units as the fuel.
`CompilerSession::set_run_limits` gives the artifacts a session compiles
their limits (a compile's run of `main` included), and
`SessionArtifacts::set_run_limits` and `SessionRuntime::set_run_limits`
change them for later calls. Running out of fuel traps. A call past the
call depth signals `<stack-overflow-error>` instead, which a program can
handle: `execution_tape` throws a tag at the call
(`Limits::call_depth_exception`), and the innermost region around it (every
function and closure body has one, as do `block` and `handle`) catches it
and signals with that frame's handlers, exits and cleanups still in place;
handlers and guards get 4,096 frames of headroom
(`driver::STACK_OVERFLOW_HEADROOM`) above the stack that ran out. Once the
stack is back under the depth, the next overflow signals again; a handler
that needs more than the headroom traps.

Compiler error and trace wrappers containing `TrapInfo` implement `Clone` and
`Debug`, and no longer derive `Eq`/`PartialEq`: traps may carry an owned typed
host error. Match the error variant or inspect its cause instead of comparing
whole errors. `FunctionCallError::Trap` owns its trap in a box to keep normal
error results compact. `lower::Function::register_kinds` contains declarations
for registers r1 onward; r0 remains the effect token.

Traced runs record with `execution_tape::recording::Recorder`; cairn has no
trace sink of its own. `ExecutionTrace` shares the owned upstream `Recording`
(invocation values, host calls with guest call stacks, the final return or
trap with its stack pinned before unwinding, optional instruction history,
capture completeness, and checked replay). `ExecutionTraceDetail::Boundaries`
counts every entered activation but lists only those visible at a host call
or the outcome; `ExecutionTraceDetail::Instructions` adds up to
`TRACE_INSTRUCTION_LIMIT` attempted instructions with their stacks.
`SessionArtifacts::execution_trace_view` adds Interloko meaning: tape function
ids resolve to lowered functions, their roles and environment object ids, and
instruction spans resolve to source line and column. Text dumps and HTML
reports render that view.

## Glossary

- **Loko:** the current Dylan-shaped source notation and `.loko` file format.
- **Interloko:** the compiler/runtime substrate that parses, analyzes, lowers,
  executes, and inspects Loko programs.
- **MOP:** Metaobject Protocol; structured facts about classes, slots, generic
  functions, methods, and dispatch.
- **EOP:** Environment Object Protocol; structured facts that make compiler,
  runtime, host, and future workspace objects inspectable.
- **Host-backed object:** a Rust-owned value exposed to Interloko through
  explicit metadata and runtime adapters.
- **Vivarium:** the future collaborative environment layer, not implemented in
  this repository yet.
