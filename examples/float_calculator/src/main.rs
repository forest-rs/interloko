//! Float calculator Interloko example.
//!
//! With no arguments, prints the formulas defined in
//! `programs/float_calculator.loko`. With arguments, evaluates them as one
//! staged-calculator expression, for example:
//!
//! ```text
//! cargo run -p interloko_float_calculator_example -- "sqrt(2.0) * pi"
//! ```

use interloko_compiler::session::{
    CompilerSession, DumpStage, FunctionValue, RuntimeMode, SessionArtifacts, TraceMask,
};
use interloko_diagnostics::DiagnosticRenderer;

const PROGRAM: &str = include_str!("../programs/float_calculator.loko");

/// Formulas defined by the program's staged calculator region.
const FORMULAS: &[&str] = &[
    "hypotenuse",
    "golden-ratio",
    "circle-area",
    "monthly-growth",
    "half-life",
    "overflow",
    "undefined",
];

/// Name given to an expression read from the command line.
const INPUT_FUNCTION: &str = "answer";

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.first().map(String::as_str) {
        None => run_formulas(),
        Some("--dump-lowered") if args.len() == 1 => dump(DumpStage::Lowered),
        Some("--dump-tape") if args.len() == 1 => dump(DumpStage::Tape),
        Some(flag) if flag.starts_with("--") => Err(String::from(
            "usage: cargo run -p interloko_float_calculator_example -- \
             [--dump-lowered|--dump-tape|EXPRESSION...]",
        )),
        Some(_) => evaluate(&args.join(" ")),
    };
    if let Err(message) = result {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn run_formulas() -> Result<(), String> {
    let artifacts = compile(PROGRAM)?;
    for name in FORMULAS {
        println!("{name} = {}", call(&artifacts, name, &[])?);
    }
    match call(&artifacts, "summary", &[])? {
        FunctionValue::String(text) => println!("{text}"),
        other => println!("{other}"),
    }
    println!(
        "whole-days(100) = {}",
        call(&artifacts, "whole-days", &[FunctionValue::Integer(100)])?
    );
    Ok(())
}

fn evaluate(expression: &str) -> Result<(), String> {
    let source = format!(
        "{PROGRAM}\n// staged-loko: calculator\n{INPUT_FUNCTION} = {expression}\n// end-staged-loko\n"
    );
    let artifacts = compile(&source)?;
    println!("{}", call(&artifacts, INPUT_FUNCTION, &[])?);
    Ok(())
}

fn dump(stage: DumpStage) -> Result<(), String> {
    print!("{}", compile(PROGRAM)?.dump_stage(stage));
    Ok(())
}

fn compile(source: &str) -> Result<SessionArtifacts, String> {
    let artifacts =
        CompilerSession::compile_source("float_calculator.loko", source, RuntimeMode::CompileOnly);
    if artifacts.diagnostics.is_empty() {
        return Ok(artifacts);
    }
    let renderer = DiagnosticRenderer::english().map_err(|error| error.to_string())?;
    let mut message = String::from("the expression did not compile:");
    for diagnostic in &artifacts.diagnostics {
        let text = renderer
            .render(diagnostic)
            .unwrap_or_else(|_| diagnostic.code.to_string());
        message.push_str(&format!("\n  {text}"));
    }
    Err(message)
}

fn call(
    artifacts: &SessionArtifacts,
    name: &str,
    arguments: &[FunctionValue],
) -> Result<FunctionValue, String> {
    artifacts
        .call_function(name, arguments, TraceMask::NONE)
        .map(|outcome| outcome.value)
        .map_err(|error| format!("{name}: {error}"))
}
