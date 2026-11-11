//! Generic-dispatch Interloko example: `main` dispatches `area` on a
//! `<circle>`.

use std::error::Error;

use interloko_compiler::session::{CompilerSession, DumpStage, FunctionValue, RuntimeMode};

const NAME: &str = "generic_area.loko";
const SOURCE: &str = include_str!("../programs/generic_area.loko");

fn main() -> Result<(), Box<dyn Error>> {
    let artifacts = CompilerSession::compile_source(NAME, SOURCE, RuntimeMode::CompileOnly);
    if artifacts.has_errors() {
        let codes = artifacts
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.is_error())
            .map(|diagnostic| diagnostic.code.to_string())
            .collect::<Vec<_>>();
        return Err(format!("{NAME} did not compile: {}", codes.join(", ")).into());
    }

    match dump_mode().as_deref() {
        None => {
            let value = artifacts.call_function("main", &[], None)?.value;
            assert_eq!(value, FunctionValue::Integer(100));
            println!("{value}");
        }
        Some("--dump-lowered") => print!("{}", artifacts.dump_stage(DumpStage::Lowered)),
        Some("--dump-tape") => print!("{}", artifacts.dump_stage(DumpStage::Tape)),
        Some("--dump-all") => {
            print!("{}", artifacts.dump_stage(DumpStage::Lowered));
            println!();
            print!("{}", artifacts.dump_stage(DumpStage::Tape));
        }
        Some(_) => {
            eprintln!(
                "usage: cargo run -p interloko_generic_area_example -- [--dump-lowered|--dump-tape|--dump-all]"
            );
            std::process::exit(2);
        }
    }

    Ok(())
}

fn dump_mode() -> Option<String> {
    let mut args = std::env::args().skip(1);
    let mode = args.next();
    if args.next().is_some() {
        return Some(String::new());
    }
    mode
}
