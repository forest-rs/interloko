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

define function plain(d :: <integer>) => <integer>
  if d = 0 then 1 else plain(d - 1) + plain(d - 1) end
end;

define function numeric(d :: <integer>) => <integer>
  if d = 0 then as(<integer>, as(<object>, d) + 1) else numeric(d - 1) + numeric(d - 1) end
end;

define function narrowed(d :: <integer>, s :: <shape>) => <integer>
  if d = 0 then
    if instance?(s, <circle>) then area(s) else 0 end
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
  if d = 0 then make-shape() else made(d - 1) + made(d - 1) end
end;

define function cast(d :: <integer>) => <integer>
  if d = 0 then cast-shape() else cast(d - 1) + cast(d - 1) end
end;

define function convert(d :: <integer>) => <integer>
  if d = 0 then as(<integer>, as(<i32>, d)) + 1 else convert(d - 1) + convert(d - 1) end
end;
";

/// Cases recurse as a balanced binary tree this deep and operate at its
/// `2^DEPTH` leaves.
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

/// Net nanoseconds per leaf operation of each case under `policy`.
fn costs(policy: CodegenPolicy) -> Vec<(&'static str, f64)> {
    let mut session = CompilerSession::new();
    session.set_codegen_policy(policy);
    let id = session.add_source("typed.loko", SOURCE);
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
    ]
}

fn main() {
    let off = costs(CodegenPolicy::Off);
    let proved = costs(CodegenPolicy::Proved);
    println!("{:<19} {:>12} {:>12}", "", "off", "proved");
    for ((label, off), (_, proved)) in off.iter().zip(&proved) {
        println!("{label:<19} {off:>9.1} ns {proved:>9.1} ns/op");
    }
}
