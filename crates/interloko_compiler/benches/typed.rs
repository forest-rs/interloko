//! Wall-clock cost of the runtime checks soft typing can prove, with and
//! without lowering acting on the proofs.
//!
//! Each case is a Loko function that does one checked operation at every
//! leaf of a balanced call tree, as in the `control` bench; `plain` walks
//! the same tree, and its time is subtracted, so the report is the cost of
//! the operation alone. Every case is proved by typing, so
//! `CodegenPolicy::Proved` drops or specializes its check:
//!
//! - `numeric dispatch`: `+` on a boxed `<object>` that holds an `<integer>`
//!   sends to a built-in numeric generic; proved code adds inline.
//! - `narrowed read`: a `<shape>` read as the `<circle>` an `instance?` test
//!   found costs a cast; proved code reuses the register.
//! - `proved cast`: `as(<circle>, s)` on a `<shape>` local holding a
//!   `<circle>`; proved code skips the class check. Its baseline makes the
//!   same object without casting it.
//! - `range conversion`: `as(<i32>, n)` on a small `<integer>`; proved code
//!   keeps the low bits instead of testing the range.
//! - `u32 conversion`: the same with `as(<u32>, n)`.
//! - `tested arithmetic`: `x * x + x + 1` on a boxed `<object>` in the then
//!   branch of `instance?(x, <integer>)`. Sem reads `x` there as an
//!   unboxed `<integer>` under either policy, so the branch unboxes once
//!   and computes on `i64`s. Its baseline boxes and passes the same value
//!   to a function that does not test it.
//! - `generic send`: `weight(c)` on a `<circle>` local sends to a generic
//!   with one method. Off asks the runtime which method to call (its cache
//!   answers after the first send) and calls it indirectly; proved calls the
//!   method through a relinkable tape link. Its baseline makes the same
//!   object and passes it to a function.
//!
//! - `exact float`: `as(<integer>, f)` on an integral `<double-float>`.
//!   Typing cannot prove a float integral, so both policies test it; the
//!   case also pays for making the float from the leaf's `<integer>`.
//! - `shift count`: `shift-left(x, n)` with an `<i32>` count typing knows
//!   nothing about, so both policies test it is in 0..=31. Its baseline
//!   calls a function with the same signature that returns `x`.
//!
//! Three cases measure what sealing buys. The `sealed` column compiles the
//! same program under `proved` with `<shape>` sealed and abstract and its
//! subclasses sealed (see [`sealed`]):
//!
//! - `shape send`: `size-of(s)` on a `<shape>` parameter, whose only
//!   method is on `<shape>`. Open, a subclass may override it, so the
//!   runtime selects; sealed, every piece is an exact class selecting that
//!   method, so the send is linked. Its baseline calls a function that
//!   does the same work.
//! - `leaf send`: `weight(c)` on a `<circle>` parameter. Open, `<circle>`
//!   may gain subclasses; sealed with none, it is exact and the send is
//!   linked. Same baseline.
//! - `typecase cast`: `as(<square>, s).side` in the else branch of
//!   `instance?(s, <circle>)`. Open, the cast is checked; sealed, the else
//!   branch holds only `<square>`s and the cast is dropped. Its baseline
//!   runs the same test and reads the slot through a `<square>` parameter
//!   holding the same object.
//!
//! Four cases measure calls through callable values, each against a
//! direct call of the same function:
//!
//! - `untyped call`: `apply(f, d)` on a `<callable>` parameter holding a
//!   function value.
//! - `typed callable`: `f(d)` on a `callable(<integer>) => <integer>`
//!   parameter holding the same function.
//! - `untyped closure`: `apply(f, d)` on a `<callable>` holding an
//!   anonymous function.
//! - `mismatched call`: `apply(f, x)` on the same function value with an
//!   `<object>` argument, whose call shape is not the function's, so the
//!   argument and result cross boxed.
//!
//! Two more measure carrying a callable value through calls without calling
//! it, against `plain`: `typed passed` through a typed callable parameter,
//! `untyped passed` through a `<callable>` one.
//!
//! `loop iteration` is one iteration of a `while` loop that adds its
//! counter to a total, `2^DEPTH` of them in one frame, against a function
//! that returns at once. Under `proved` the counter's increment is proved
//! not to overflow; the total's addition is checked. `for iteration` is
//! the same loop written `for (i from 0 below 256)`, which lowers to that
//! `while`, so the two rows should agree.
//!
//! `cell access` is what one read and one write of the total cost when a
//! closure captures it, so it lives in a shared cell, against the same loop
//! with the total in a register.
//! `vector element` is one `as(<integer>, v[i])` of a `<vector>` in a loop
//! testing `i < size(v)`, against making the vector alone: a size read, a
//! checked element read and an unbox. `table lookup` is one `as(<integer>, t[i])` of a
//! `<table>` with integer keys, against filling the table alone. `vector
//! for` is `vector element`'s loop written `for (x :: <integer> in v)`,
//! which reads the vector the same way, so the two rows should agree.
//!
//! `character scan` is one iteration of a loop over a 256-character ASCII
//! `<string>` by position: `s[p]`, a `<character>` compare and
//! `next-position`, two runtime host calls. `scan + digit?` tests each
//! character with `digit?` instead, a third.
//!
//! Run with `cargo bench -p interloko_compiler --features std --bench typed`.

use std::hint::black_box;
use std::time::{Duration, Instant};

use interloko_compiler::lower::CodegenPolicy;
use interloko_compiler::session::{
    CompilerSession, FunctionValue, RuntimeMode, SessionArtifacts, SessionRuntime,
};

const SOURCE: &str = "
define class <shape> (<object>)
end;

define class <circle> (<shape>)
  slot radius :: <integer>;
end;

define class <square> (<shape>)
  slot side :: <integer>;
end;

define function plain(d :: <integer>) => <integer>
  if (d = 0) 1 else plain(d - 1) + plain(d - 1) end
end;

define function numeric(d :: <integer>) => <integer>
  if (d = 0) as(<integer>, as(<object>, d) + 1) else numeric(d - 1) + numeric(d - 1) end
end;

define function narrowed(d :: <integer>, s :: <shape>) => <integer>
  if (d = 0)
    if (instance?(s, <circle>)) area(s) else 0 end
  else
    narrowed(d - 1, s) + narrowed(d - 1, s)
  end
end;

define function area(c :: <circle>) => <integer>
  1
end;

define function narrow(d :: <integer>) => <integer>
  narrowed(d, make(<circle>, radius: 1))
end;

define function make-shape() => <integer>
  let s :: <shape> = make(<circle>, radius: 1);
  1
end;

define function cast-shape() => <integer>
  let s :: <shape> = make(<circle>, radius: 1);
  area(as(<circle>, s))
end;

define function made(d :: <integer>) => <integer>
  if (d = 0) make-shape() else made(d - 1) + made(d - 1) end
end;

define function cast(d :: <integer>) => <integer>
  if (d = 0) cast-shape() else cast(d - 1) + cast(d - 1) end
end;

define function poly(x :: <object>) => <integer>
  if (instance?(x, <integer>)) as(<integer>, x * x + x + 1) else 0 end
end;

define function ignore(x :: <object>) => <integer>
  1
end;

define function tested(d :: <integer>) => <integer>
  if (d = 0) poly(as(<object>, d)) else tested(d - 1) + tested(d - 1) end
end;

define function passed(d :: <integer>) => <integer>
  if (d = 0) ignore(as(<object>, d)) else passed(d - 1) + passed(d - 1) end
end;

define generic weight(s :: <shape>) => <integer>;

define method weight(c :: <circle>) => <integer>
  1
end;

define function send-weight() => <integer>
  let c = make(<circle>, radius: 1);
  weight(c)
end;

define function call-area() => <integer>
  let c = make(<circle>, radius: 1);
  area(c)
end;

define function sent(d :: <integer>) => <integer>
  if (d = 0) send-weight() else sent(d - 1) + sent(d - 1) end
end;

define function called(d :: <integer>) => <integer>
  if (d = 0) call-area() else called(d - 1) + called(d - 1) end
end;

define function convert(d :: <integer>) => <integer>
  if (d = 0) as(<integer>, as(<i32>, d)) + 1 else convert(d - 1) + convert(d - 1) end
end;

define function convert-unsigned(d :: <integer>) => <integer>
  if (d = 0)
    as(<integer>, as(<u32>, d)) + 1
  else
    convert-unsigned(d - 1) + convert-unsigned(d - 1)
  end
end;

define function exact(d :: <integer>) => <integer>
  if (d = 0) as(<integer>, as(<double-float>, d)) + 1 else exact(d - 1) + exact(d - 1) end
end;

define function shift-by(x :: <i32>, n :: <i32>) => <i32>
  shift-left(x, n)
end;

define function keep(x :: <i32>, n :: <i32>) => <i32>
  x
end;

define generic size-of(s :: <shape>) => <integer>;

define method size-of(s :: <shape>) => <integer>
  1
end;

define function send-size(s :: <shape>) => <integer>
  size-of(s)
end;

define function size-direct(s :: <shape>) => <integer>
  1
end;

define function take-shape(s :: <shape>) => <integer>
  size-direct(s)
end;

define function shape-sent(d :: <integer>) => <integer>
  if (d = 0) send-size(make(<circle>, radius: 1)) else shape-sent(d - 1) + shape-sent(d - 1) end
end;

define function shape-called(d :: <integer>) => <integer>
  if (d = 0) take-shape(make(<circle>, radius: 1)) else shape-called(d - 1) + shape-called(d - 1) end
end;

define function send-leaf(c :: <circle>) => <integer>
  weight(c)
end;

define function weight-direct(c :: <circle>) => <integer>
  1
end;

define function take-circle(c :: <circle>) => <integer>
  weight-direct(c)
end;

define function leaf-sent(d :: <integer>) => <integer>
  if (d = 0) send-leaf(make(<circle>, radius: 1)) else leaf-sent(d - 1) + leaf-sent(d - 1) end
end;

define function leaf-called(d :: <integer>) => <integer>
  if (d = 0) take-circle(make(<circle>, radius: 1)) else leaf-called(d - 1) + leaf-called(d - 1) end
end;

define function square-side(s :: <shape>) => <integer>
  if (instance?(s, <circle>)) 0 else as(<square>, s).side end
end;

define function square-test(s :: <shape>, q :: <square>) => <integer>
  if (instance?(s, <circle>)) 0 else q.side end
end;

define function test-square(q :: <square>) => <integer>
  square-test(q, q)
end;

define function side-square(q :: <square>) => <integer>
  square-side(q)
end;

define function cast-past(d :: <integer>) => <integer>
  if (d = 0) side-square(make(<square>, side: 1)) else cast-past(d - 1) + cast-past(d - 1) end
end;

define function test-only(d :: <integer>) => <integer>
  if (d = 0) test-square(make(<square>, side: 1)) else test-only(d - 1) + test-only(d - 1) end
end;

define function shifted(d :: <integer>) => <integer>
  if (d = 0) as(<integer>, shift-by(1i32, 3i32)) else shifted(d - 1) + shifted(d - 1) end
end;

define function kept(d :: <integer>) => <integer>
  if (d = 0) as(<integer>, keep(1i32, 3i32)) else kept(d - 1) + kept(d - 1) end
end;

define function inc(n :: <integer>) => <integer>
  n + 1
end;

define function inc-called(d :: <integer>) => <integer>
  if (d = 0) inc(d) else inc-called(d - 1) + inc-called(d - 1) end
end;

define function untyped-tree(f :: <callable>, d :: <integer>) => <integer>
  if (d = 0) apply(f, d) else untyped-tree(f, d - 1) + untyped-tree(f, d - 1) end
end;

define function untyped(d :: <integer>) => <integer>
  untyped-tree(inc, d)
end;

define function typed-tree(f :: callable(<integer>) => <integer>, d :: <integer>) => <integer>
  if (d = 0) f(d) else typed-tree(f, d - 1) + typed-tree(f, d - 1) end
end;

define function typed-callable(d :: <integer>) => <integer>
  typed-tree(inc, d)
end;

define function closure-tree(f :: <callable>, d :: <integer>) => <integer>
  if (d = 0) apply(f, d) else closure-tree(f, d - 1) + closure-tree(f, d - 1) end
end;

define function closure-call(d :: <integer>) => <integer>
  closure-tree(function (n :: <integer>) => <integer> n + 1 end, d)
end;

define function mismatched-tree(f :: <callable>, x :: <object>, d :: <integer>) => <integer>
  if (d = 0) apply(f, x) else mismatched-tree(f, x, d - 1) + mismatched-tree(f, x, d - 1) end
end;

define function mismatched(d :: <integer>) => <integer>
  mismatched-tree(inc, as(<object>, d), d)
end;

define function typed-carry(f :: callable(<integer>) => <integer>, d :: <integer>) => <integer>
  if (d = 0) 1 else typed-carry(f, d - 1) + typed-carry(f, d - 1) end
end;

define function typed-passed(d :: <integer>) => <integer>
  typed-carry(inc, d)
end;

define function untyped-carry(f :: <callable>, d :: <integer>) => <integer>
  if (d = 0) 1 else untyped-carry(f, d - 1) + untyped-carry(f, d - 1) end
end;

define function untyped-passed(d :: <integer>) => <integer>
  untyped-carry(inc, d)
end;

define function looped(d :: <integer>) => <integer>
  let total = 0;
  let i = 0;
  while (i < 256)
    total := total + i;
    i := i + 1
  end;
  total
end;

define function for-looped(d :: <integer>) => <integer>
  let total = 0;
  for (i from 0 below 256)
    total := total + i
  end;
  total
end;

define function unlooped(d :: <integer>) => <integer>
  0
end;

define function cell-looped(d :: <integer>) => <integer>
  let total = 0;
  let read-total = function () => <integer> total end;
  let i = 0;
  while (i < 256)
    total := total + i;
    i := i + 1
  end;
  read-total()

end;

define function vector-made(d :: <integer>) => <integer>
  let v = make(<vector>, size: 256, fill: 1);
  size(v)
end;

define function vector-read(d :: <integer>) => <integer>
  let v = make(<vector>, size: 256, fill: 1);
  let total = 0;
  let i = 0;
  while (i < size(v))
    total := total + as(<integer>, v[i]);
    i := i + 1
  end;
  total
end;

define function vector-for(d :: <integer>) => <integer>
  let v = make(<vector>, size: 256, fill: 1);
  let total = 0;
  for (x :: <integer> in v)
    total := total + x
  end;
  total
end;

define function table-filled(d :: <integer>) => <integer>
  let t = make(<table>);
  let i = 0;
  while (i < 256)
    t[i] := i;
    i := i + 1
  end;
  size(t)
end;

define function table-read(d :: <integer>) => <integer>
  let t = make(<table>);
  let i = 0;
  while (i < 256)
    t[i] := i;
    i := i + 1
  end;
  let total = 0;
  let j = 0;
  while (j < 256)
    total := total + as(<integer>, t[j]);
    j := j + 1
  end;
  total
end;

define function scanned(d :: <integer>) => <integer>
  let s = \"let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = \";
  let p = start-position(s);
  let stop = end-position(s);
  let n = 0;
  while (p < stop)
    if (s[p] = ' ') n := n + 1 end;
    p := next-position(s, p)
  end;
  n
end;

define function scanned-digits(d :: <integer>) => <integer>
  let s = \"let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = 42; let x1 = \";
  let p = start-position(s);
  let stop = end-position(s);
  let n = 0;
  while (p < stop)
    if (digit?(s[p])) n := n + 1 end;
    p := next-position(s, p)
  end;
  n
end;
";

/// Cases recurse as a balanced binary tree this deep and operate at its
/// `2^DEPTH` leaves; `looped` and `for-looped` iterate `2^DEPTH` times.
const DEPTH: i64 = 8;
const LEAVES: f64 = 256.0;
const SAMPLES: usize = 15;
const TARGET: Duration = Duration::from_millis(40);

fn call(artifacts: &SessionArtifacts, runtime: &mut SessionRuntime, name: &str) {
    let outcome = runtime
        .call_function(artifacts, name, &[FunctionValue::Integer(DEPTH)], None)
        .unwrap_or_else(|error| panic!("{name} failed: {error}"));
    black_box(outcome);
}

/// Median wall time of one call of `name`.
fn measure(artifacts: &SessionArtifacts, runtime: &mut SessionRuntime, name: &str) -> Duration {
    let start = Instant::now();
    let mut calls = 0_u32;
    while start.elapsed() < TARGET / 4 {
        call(artifacts, runtime, name);
        calls += 1;
    }
    let per_sample = (calls * 4 / SAMPLES as u32).max(1);
    let mut samples = (0..SAMPLES)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..per_sample {
                call(artifacts, runtime, name);
            }
            start.elapsed() / per_sample
        })
        .collect::<Vec<_>>();
    samples.sort();
    samples[SAMPLES / 2]
}

/// [`SOURCE`] with `<shape>` sealed and abstract and its subclasses
/// sealed, so a `<shape>` is exactly a `<circle>` or a `<square>`.
fn sealed() -> String {
    SOURCE
        .replace(
            "define class <shape> (<object>)",
            "define sealed abstract class <shape> (<object>)",
        )
        .replace(
            "define class <circle> (<shape>)",
            "define sealed class <circle> (<shape>)",
        )
        .replace(
            "define class <square> (<shape>)",
            "define sealed class <square> (<shape>)",
        )
}

/// Net nanoseconds per leaf operation of each case of `source` under
/// `policy`.
fn costs(source: &str, policy: CodegenPolicy) -> Vec<(&'static str, f64)> {
    let mut session = CompilerSession::new();
    session.set_codegen_policy(policy);
    let id = session.add_source("typed.loko", source);
    let artifacts = session
        .compile(id, RuntimeMode::CompileOnly)
        .expect("source was just added");
    assert!(!artifacts.has_errors(), "the benchmark program compiles");
    let mut runtime = artifacts.runtime().expect("a runtime image");
    let nanos = |duration: Duration| duration.as_secs_f64() * 1e9;
    let mut net = |name: &str, baseline: &str| {
        let total = measure(&artifacts, &mut runtime, name);
        let base = measure(&artifacts, &mut runtime, baseline);
        nanos(total.saturating_sub(base)) / LEAVES
    };
    vec![
        ("numeric dispatch", net("numeric", "plain")),
        ("narrowed read", net("narrow", "plain")),
        ("proved cast", net("cast", "made")),
        ("range conversion", net("convert", "plain")),
        ("u32 conversion", net("convert-unsigned", "plain")),
        ("exact float", net("exact", "plain")),
        ("shift count", net("shifted", "kept")),
        ("tested arithmetic", net("tested", "passed")),
        ("generic send", net("sent", "called")),
        ("shape send", net("shape-sent", "shape-called")),
        ("leaf send", net("leaf-sent", "leaf-called")),
        ("typecase cast", net("cast-past", "test-only")),
        ("untyped call", net("untyped", "inc-called")),
        ("typed callable", net("typed-callable", "inc-called")),
        ("untyped closure", net("closure-call", "inc-called")),
        ("mismatched call", net("mismatched", "inc-called")),
        ("typed passed", net("typed-passed", "plain")),
        ("untyped passed", net("untyped-passed", "plain")),
        ("loop iteration", net("looped", "unlooped")),
        ("cell access", net("cell-looped", "looped")),
        ("character scan", net("scanned", "unlooped")),
        ("scan + digit?", net("scanned-digits", "unlooped")),
        ("for iteration", net("for-looped", "unlooped")),
        ("vector element", net("vector-read", "vector-made")),
        ("vector for", net("vector-for", "vector-made")),
        ("table lookup", net("table-read", "table-filled")),
    ]
}

fn main() {
    let off = costs(SOURCE, CodegenPolicy::Off);
    let proved = costs(SOURCE, CodegenPolicy::Proved);
    let sealed = costs(&sealed(), CodegenPolicy::Proved);
    println!("{:<19} {:>12} {:>12} {:>12}", "", "off", "proved", "sealed");
    for (((label, off), (_, proved)), (_, sealed)) in off.iter().zip(&proved).zip(&sealed) {
        println!("{label:<19} {off:>9.1} ns {proved:>9.1} ns {sealed:>9.1} ns/op");
    }
}
