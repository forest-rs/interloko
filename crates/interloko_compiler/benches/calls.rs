//! Wall-clock cost of one embedding call, and of the VM set-up inside it.
//!
//! A fresh VM has to define the control tags and install the program before
//! it runs anything. A runtime image keeps its VM between calls; a call on a
//! fresh image builds one. This reports both for a function that returns a
//! constant, next to that set-up alone, for a one-function program and for
//! one with many classes, generics and functions, since installing interns
//! the program's types and signatures, under each codegen policy.
//! Run with `cargo bench -p interloko_compiler --features std --bench calls`.

use std::fmt::Write as _;
use std::hint::black_box;
use std::time::{Duration, Instant};

use execution_tape::host::{Host, HostContext, HostError, SigHash, ValueRef};
use execution_tape::value::Value;
use execution_tape::vm::{Limits, Vm};
use interloko_compiler::lower::CodegenPolicy;
use interloko_compiler::session::{CompilerSession, RuntimeMode, SessionArtifacts};
use interloko_runtime::driver;

const SAMPLES: usize = 15;
const TARGET: Duration = Duration::from_millis(60);

/// A host for set-up alone: nothing runs, so nothing calls it.
struct NoHost;

impl Host for NoHost {
    fn call(
        &mut self,
        _symbol: &str,
        _sig_hash: SigHash,
        _args: &[ValueRef<'_>],
        _rets: &mut [Value],
        _context: HostContext<'_, '_>,
    ) -> Result<u64, HostError> {
        Err(HostError::UnknownSymbol)
    }
}

/// A program of `classes` classes, each with a slot, a method on one generic
/// and a function that reads it, plus `one`, the function every case calls.
fn program(classes: usize) -> String {
    let mut source = String::from(
        "define class <base> (<object>)\nend;\n\
         define generic weight(b :: <base>) => <integer>;\n\
         define function one() => <integer>\n  1\nend;\n",
    );
    for index in 0..classes {
        let _ = write!(
            source,
            "define class <c{index}> (<base>)\n  slot s{index} :: <integer>;\nend;\n\
             define method weight(b :: <c{index}>) => <integer>\n  b.s{index}\nend;\n\
             define function f{index}(n :: <integer>) => <integer>\n  \
               weight(make(<c{index}>, s{index}: n)) + {index}\nend;\n"
        );
    }
    source
}

/// Median wall time of one `run`.
fn measure(mut run: impl FnMut()) -> Duration {
    let start = Instant::now();
    let mut runs = 0_u32;
    while start.elapsed() < TARGET / 4 {
        run();
        runs += 1;
    }
    let per_sample = (runs * 4 / SAMPLES as u32).max(1);
    let mut samples = (0..SAMPLES)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..per_sample {
                run();
            }
            start.elapsed() / per_sample
        })
        .collect::<Vec<_>>();
    samples.sort();
    samples[SAMPLES / 2]
}

fn report(label: &str, artifacts: &SessionArtifacts) {
    let code = artifacts.code_unit().expect("tape");
    let mut runtime = artifacts.runtime().expect("a runtime image");
    let call = measure(|| {
        black_box(runtime.call_function(artifacts, "one", &[], None)).expect("one returns");
    });
    let fresh = measure(|| {
        black_box(artifacts.call_function("one", &[], None)).expect("one returns");
    });
    let vm = measure(|| {
        black_box(Vm::new(NoHost, Limits::default()));
    });
    let setup = measure(|| {
        let mut vm = Vm::new(NoHost, Limits::default());
        black_box(driver::install(&mut vm, &code).unwrap());
        black_box(vm);
    });
    let functions = code.program.program().functions.len();
    let nanos = |duration: Duration| duration.as_secs_f64() * 1e9;
    println!("{label} ({functions} tape functions)");
    println!("  runtime call        {:>9.0} ns", nanos(call));
    println!("  fresh-image call    {:>9.0} ns", nanos(fresh));
    println!(
        "  vm new + tags + install {:>5.0} ns ({:.0}% of a fresh-image call)",
        nanos(setup),
        100.0 * setup.as_secs_f64() / fresh.as_secs_f64()
    );
    println!("  vm new alone        {:>9.0} ns", nanos(vm));
}

fn main() {
    for policy in CodegenPolicy::ALL {
        for (label, classes) in [("one function", 0), ("64 classes", 64)] {
            let mut session = CompilerSession::new();
            session.set_codegen_policy(*policy);
            let id = session.add_source("calls.loko", &program(classes));
            let artifacts = session
                .compile(id, RuntimeMode::CompileOnly)
                .expect("source was just added");
            assert!(!artifacts.has_errors(), "the benchmark program compiles");
            report(&format!("{label}, codegen {}", policy.as_str()), &artifacts);
        }
    }
}
