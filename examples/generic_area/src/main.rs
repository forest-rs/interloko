//! Generic-dispatch Interloko example.

use interloko_compiler::driver::{CompileError, compile, compile_and_run_main};

fn main() -> Result<(), CompileError> {
    let source = include_str!("../programs/generic_area.loko");

    match dump_mode().as_deref() {
        None => {
            let value = compile_and_run_main(source)?;
            assert_eq!(value, 100);
            println!("{value}");
        }
        Some("--dump-lowered") => {
            print!("{}", compile(source)?.dump_lowered());
        }
        Some("--dump-tape") => {
            print!("{}", compile(source)?.dump_tape());
        }
        Some("--dump-all") => {
            let compiled = compile(source)?;
            print!("{}", compiled.dump_lowered());
            println!();
            print!("{}", compiled.dump_tape());
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
