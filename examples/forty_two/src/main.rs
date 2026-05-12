//! Milestone-0 end-to-end Cairn example.

use cairn_compiler::driver::{CompileError, compile_and_run_main};

fn main() -> Result<(), CompileError> {
    let source = include_str!("../programs/forty_two.cairn");
    let value = compile_and_run_main(source)?;
    assert_eq!(value, 42);
    println!("{value}");
    Ok(())
}
