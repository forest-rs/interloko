//! Generic-dispatch Cairn example.

use cairn_compiler::driver::{CompileError, compile_and_run_main};

fn main() -> Result<(), CompileError> {
    let source = include_str!("../programs/generic_area.cairn");
    let value = compile_and_run_main(source)?;
    assert_eq!(value, 100);
    println!("{value}");
    Ok(())
}
