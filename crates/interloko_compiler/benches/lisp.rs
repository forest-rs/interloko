//! What one interpreted procedure call costs in object-heavy Loko code.
//!
//! `lisp.loko` is a small Lisp interpreter written in Loko: a reader, an
//! evaluator over environment and closure objects, and a printer. Each case
//! reads and runs `(define (fib n) …) (fib n)` in it as one runtime turn.
//! `(fib n)` makes `2 * fib(n + 1) - 1` Lisp procedure calls, so the
//! difference between two cases, divided by the difference in calls, is
//! the cost of one Lisp call with reading, printing and the global
//! environment's setup taken out.
//!
//! The report gives, per Lisp call:
//!
//! - wall time, from `(fib 16)` against `(fib 1)`;
//! - runtime host calls, from traced runs of `(fib 5)` against `(fib 2)`,
//!   with the host functions that make up most of them.
//!
//! Run with `cargo bench -p interloko_compiler --features std --bench lisp`.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::{Duration, Instant};

use interloko_compiler::session::{
    CompilerSession, ExecutionTraceDetail, FunctionValue, RuntimeMode, SessionArtifacts,
    SessionRuntime,
};

const SOURCE: &str = include_str!("lisp.loko");
const SAMPLES: usize = 15;
const TARGET: Duration = Duration::from_millis(400);

/// Lisp procedure calls `(fib n)` makes.
fn lisp_calls(n: u32) -> u64 {
    let (mut a, mut b) = (0_u64, 1_u64);
    for _ in 0..n + 1 {
        (a, b) = (b, a + b);
    }
    2 * a - 1
}

fn call(artifacts: &SessionArtifacts, runtime: &mut SessionRuntime, name: &str) {
    let outcome = runtime
        .call_function(artifacts, name, &[], None)
        .unwrap_or_else(|error| panic!("{name} failed: {error}"));
    black_box(outcome);
}

/// Checks that the interpreter computes `(fib n)`, so the bench measures a working one.
fn check(artifacts: &SessionArtifacts, name: &str, expected: &str) {
    let outcome = artifacts
        .call_function(name, &[], None)
        .unwrap_or_else(|error| panic!("{name} failed: {error}"));
    assert_eq!(
        outcome.value,
        FunctionValue::String(expected.into()),
        "{name} computes its Fibonacci number"
    );
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

/// The host calls one traced call of `name` makes, by host function.
fn host_calls(artifacts: &SessionArtifacts, name: &str) -> BTreeMap<String, u64> {
    let outcome = artifacts
        .call_function(name, &[], Some(ExecutionTraceDetail::Boundaries))
        .unwrap_or_else(|error| panic!("{name} failed: {error}"));
    let trace = outcome.trace.expect("a traced call returns its trace");
    assert!(
        trace.recording().status().is_ok(),
        "the trace of {name} holds every host call"
    );
    let view = artifacts.execution_trace_view(&trace);
    let mut counts = BTreeMap::new();
    for host_call in view.host_calls() {
        let symbol = host_call.symbol.unwrap_or("?").to_string();
        *counts.entry(symbol).or_insert(0) += 1;
    }
    counts
}

fn main() {
    let mut session = CompilerSession::new();
    let id = session.add_source("lisp.loko", SOURCE);
    let artifacts = session
        .compile(id, RuntimeMode::CompileOnly)
        .expect("source was just added");
    assert!(!artifacts.has_errors(), "the benchmark program compiles");
    for (name, expected) in [
        ("fib-1", "1"),
        ("fib-2", "1"),
        ("fib-5", "5"),
        ("fib-16", "987"),
    ] {
        check(&artifacts, name, expected);
    }
    let mut runtime = artifacts.runtime().expect("a runtime image");

    let nanos = |duration: Duration| duration.as_secs_f64() * 1e9;
    let small = measure(&artifacts, &mut runtime, "fib-1");
    let large = measure(&artifacts, &mut runtime, "fib-16");
    let calls = (lisp_calls(16) - lisp_calls(1)) as f64;
    println!(
        "(fib 16): {:.2} ms, {} Lisp calls",
        nanos(large) / 1e6,
        lisp_calls(16)
    );
    println!(
        "per Lisp call: {:.0} ns",
        nanos(large.saturating_sub(small)) / calls
    );

    let before = host_calls(&artifacts, "fib-2");
    let after = host_calls(&artifacts, "fib-5");
    let calls = (lisp_calls(5) - lisp_calls(2)) as f64;
    let mut per_call = after
        .iter()
        .map(|(symbol, count)| {
            let base = before.get(symbol).copied().unwrap_or(0);
            (symbol.as_str(), count.saturating_sub(base) as f64 / calls)
        })
        .filter(|(_, count)| *count > 0.0)
        .collect::<Vec<_>>();
    per_call.sort_by(|a, b| b.1.total_cmp(&a.1));
    let total = per_call.iter().map(|(_, count)| count).sum::<f64>();
    println!("host calls per Lisp call: {total:.1}");
    for (symbol, count) in per_call {
        println!("  {symbol:<40} {count:>7.1}");
    }
}
