//! Wall-clock cost of dynamic control: establishing handlers, signalling
//! conditions on each path a signal can take, and generic sends.
//!
//! Each case is a Loko function that does one operation at every leaf of a
//! balanced call tree. A call runs as one runtime turn; `plain` walks the
//! same tree with no operation, and its time is subtracted, so the report is
//! the cost of the operation alone. One more case establishes handlers under
//! a deep stack, because control requests look at the live frames. Unhandled
//! signals are resumed with a value by a chooser. Run with
//! `cargo bench -p interloko_compiler --features std --bench control`.

use std::hint::black_box;
use std::time::{Duration, Instant};

use execution_tape::value::Value;
use interloko_compiler::session::{
    CallServices, CompilerSession, FunctionValue, RuntimeMode, SessionArtifacts, SessionRuntime,
};
use interloko_runtime::chooser::{Choice, ConditionChooser, UnhandledCondition};

const SOURCE: &str = "
define class <problem> (<object>)
end;

define class <shape> (<object>)
  slot side :: <integer>;
end;

define generic area(s :: <shape>) => <integer>;

define method area(s :: <shape>) => <integer>
  s.side
end;

define function risky() => <integer>
  signal(make(<problem>))
end;

define function plain(d :: <integer>) => <integer>
  if d = 0 then 1 else plain(d - 1) + plain(d - 1) end
end;

define function establish(d :: <integer>) => <integer>
  if d = 0 then
    handle 1 with restart <problem> 2 end
  else
    establish(d - 1) + establish(d - 1)
  end
end;

define function handled(d :: <integer>) => <integer>
  if d = 0 then
    handle risky() with restart <problem> 1 end
  else
    handled(d - 1) + handled(d - 1)
  end
end;

define function guarded(d :: <integer>) => <integer>
  if d = 0 then
    handle risky() with restart <problem> when d = 0 => 1 end
  else
    guarded(d - 1) + guarded(d - 1)
  end
end;

define function unhandled(d :: <integer>) => <integer>
  if d = 0 then risky() else unhandled(d - 1) + unhandled(d - 1) end
end;

define function sends(d :: <integer>, s :: <shape>) => <integer>
  if d = 0 then area(s) else sends(d - 1, s) + sends(d - 1, s) end
end;

define function send(d :: <integer>) => <integer>
  sends(d, make(<shape>, side: 1))
end;

define function chain(n :: <integer>) => <integer>
  if n = 0 then 0 else 1 + chain(n - 1) end
end;

define function deep(n :: <integer>) => <integer>
  if n = 0 then 0 else (handle 1 with restart <problem> 2 end) + deep(n - 1) end
end;
";

/// Cases recurse as a balanced binary tree this deep and operate at its
/// `2^DEPTH` leaves, so the stack stays shallow and per-operation costs do
/// not depend on how many frames are live.
const DEPTH: i64 = 8;
const LEAVES: f64 = 256.0;
/// Frames under each operation of the deep case.
const DEEP: i64 = 200;
const SAMPLES: usize = 15;
const TARGET: Duration = Duration::from_millis(40);

/// Resumes every unhandled condition with `1`.
struct UseOne;

impl ConditionChooser for UseOne {
    fn choose(&mut self, _condition: &UnhandledCondition<'_>) -> Choice {
        Choice::UseValue(vec![Value::I64(1)])
    }
}

fn call(artifacts: &SessionArtifacts, runtime: &mut SessionRuntime, name: &str, arg: i64) {
    let mut chooser = UseOne;
    let outcome = runtime
        .call_function_with(
            artifacts,
            name,
            &[FunctionValue::Integer(arg)],
            None,
            CallServices {
                host_functions: None,
                chooser: Some(&mut chooser),
            },
        )
        .unwrap_or_else(|error| panic!("{name} failed: {error}"));
    black_box(outcome);
}

/// Median wall time of one call of `name`.
fn measure(
    artifacts: &SessionArtifacts,
    runtime: &mut SessionRuntime,
    name: &str,
    arg: i64,
) -> Duration {
    let start = Instant::now();
    let mut calls = 0_u32;
    while start.elapsed() < TARGET / 4 {
        call(artifacts, runtime, name, arg);
        calls += 1;
    }
    let per_sample = (calls * 4 / SAMPLES as u32).max(1);
    let mut samples = (0..SAMPLES)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..per_sample {
                call(artifacts, runtime, name, arg);
            }
            start.elapsed() / per_sample
        })
        .collect::<Vec<_>>();
    samples.sort();
    samples[SAMPLES / 2]
}

fn main() {
    let artifacts =
        CompilerSession::compile_source("control.loko", SOURCE, RuntimeMode::CompileOnly);
    assert!(!artifacts.has_errors(), "the benchmark program compiles");
    let mut runtime = artifacts.runtime().expect("a runtime image");
    let nanos = |duration: Duration| duration.as_secs_f64() * 1e9;
    let baseline = measure(&artifacts, &mut runtime, "plain", DEPTH);
    println!(
        "plain               {:>9.1} ns/call ({LEAVES} leaves, subtracted below)",
        nanos(baseline)
    );
    for (name, label) in [
        ("establish", "handler-establish"),
        ("handled", "signal (handled)"),
        ("guarded", "signal (guarded)"),
        ("unhandled", "signal (use-value)"),
        ("send", "send"),
    ] {
        let total = measure(&artifacts, &mut runtime, name, DEPTH);
        let net = nanos(total.saturating_sub(baseline)) / LEAVES;
        println!("{label:<19} {net:>9.1} ns/op");
    }
    let chain = measure(&artifacts, &mut runtime, "chain", DEEP);
    let deep = measure(&artifacts, &mut runtime, "deep", DEEP);
    let net = nanos(deep.saturating_sub(chain)) / DEEP as f64;
    println!(
        "{:<19} {net:>9.1} ns/op ({DEEP} frames deep)",
        "handler-establish"
    );
}
