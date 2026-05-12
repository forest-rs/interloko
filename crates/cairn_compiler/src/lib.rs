//! Milestone-0 Cairn compiler frontend.
//!
//! This crate intentionally implements only the first vertical slice from the
//! repository design: a tiny Dylan-like syntax lowered to direct
//! `execution_tape` functions.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

/// Surface AST produced from the CST.
pub mod ast;
/// Shared diagnostic types.
pub mod diagnostic;
/// End-to-end compile and run helpers.
pub mod driver;
/// Semantic IR with resolved direct calls and locals.
pub mod sem;
/// Syntax and parsing support.
pub mod syntax;
/// `execution_tape` emission.
pub mod tape;
