//! Milestone-0 Interloko compiler frontend.
//!
//! This crate intentionally implements only the first vertical slice from the
//! repository design: a tiny Dylan-like syntax lowered through a small
//! executable IR to `execution_tape` functions.

#![no_std]

extern crate alloc;

#[cfg(any(test, feature = "std"))]
extern crate std;

/// Surface AST produced from the CST.
pub mod ast;
/// Shared diagnostic types.
pub mod diagnostic;
/// End-to-end compile and run helpers.
pub mod driver;
mod execution_trace;
/// Read-only grammar frontend projection for the base Loko parser.
pub mod grammar_frontend;
/// Stable-ish compiler identities and artifact keys.
pub mod identity;
/// Thin query invalidation shell for compiler artifacts.
pub mod incremental;
/// Backend-neutral executable IR lowered from semantic IR.
pub mod lower;
/// Runtime host support for generated `execution_tape` programs.
pub mod runtime;
/// Semantic IR with resolved direct calls and locals.
pub mod sem;
/// Compiler session API and inspectable stage artifacts.
pub mod session;
/// Inert source/frontend protocol data.
pub mod source_data;
/// Syntax and parsing support.
pub mod syntax;
/// `execution_tape` emission.
pub mod tape;
/// Durable session transcripts: structured save / open / restore of the
/// session mutation log. Requires the `std` feature for file I/O and
/// TOML serialization.
#[cfg(feature = "std")]
pub mod transcript;
pub mod typing;
