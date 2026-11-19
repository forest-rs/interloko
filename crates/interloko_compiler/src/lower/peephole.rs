//! Peephole improvements to a lowered function's instructions.
//!
//! Lowering emits each expression on its own, so a loop pays for structure
//! the tape interpreter executes one instruction at a time: a jump to a block
//! that only jumps on, a result computed into a temporary and then moved to
//! the local that holds it, and constants made again on every iteration. A
//! tape instruction costs a few nanoseconds whatever it does, so these are a
//! large share of a tight loop. This pass removes them, in three steps that
//! each preserve what every register holds wherever it is read:
//!
//! - **Jump threading.** A branch or jump to a block whose only instruction
//!   is `jmp L` goes to `L` directly. The block stays, since other control
//!   transfers may still reach it.
//! - **Move coalescing.** `t = op(...)` followed by `d = mov t` becomes
//!   `d = op(...)` when `t` is written and read nowhere else and `op` reads
//!   all its operands before it writes its result ([`writes_after_reads`]).
//! - **Constant hoisting.** A constant made inside a loop, into a register
//!   nothing else writes, is made once before the loop instead. A loop is a
//!   backward jump to a label whose only entry from outside is the jump just
//!   before it.
//! - **Unbox forwarding.** `b = box v` followed, in the same straight run
//!   of instructions with `v` and `b` unchanged, by `u = unbox b` of `v`'s
//!   own kind is `u = mov v`: the check cannot fail. A box nothing reads
//!   any more, into a register no source local names, goes too. An `as` of a
//!   boxed number, which binds the value to a hidden local and unboxes it
//!   where typing decided its class, leaves such pairs.
//! - **Decode sharing.** `s[p]` (or `element(s, p, default:)`) followed by
//!   `next-position(s, p)` decodes `p` once ([`super::DecodeShare`]), when
//!   nothing between them writes `s` or `p` and nothing outside them can
//!   reach the `next-position` without passing the read.
//!
//! Register reads and writes come from [`for_each_read`] and
//! [`for_each_write`], and label references from [`for_each_label_ref`],
//! which match every instruction exhaustively, so a new instruction must
//! say what it touches before this pass compiles.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use execution_tape::program::Reg;

use super::{BoxedEntry, Catch, DecodeShare, Function, Instr, LabelId, ValueKind};
use crate::sem;

/// Runs every peephole step over `function`.
pub(super) fn optimize(function: &mut Function) {
    forward_unboxes(function);
    thread_jumps(&mut function.instructions);
    coalesce_moves(function);
    hoist_loop_constants(function);
    share_decodes(&mut function.instructions);
}

/// Calls `f` with every register `instr` reads.
fn for_each_read(instr: &Instr, mut f: impl FnMut(Reg)) {
    let mut all = |regs: &[Reg]| regs.iter().copied().for_each(&mut f);
    match instr {
        Instr::Label { .. }
        | Instr::ConstI64 { .. }
        | Instr::ConstF64 { .. }
        | Instr::Const32 { .. }
        | Instr::ConstBool { .. }
        | Instr::StringLiteral { .. }
        | Instr::SymbolLiteral { .. }
        | Instr::ConstU64 { .. }
        | Instr::ConstClass { .. }
        | Instr::ConstFunction { .. }
        | Instr::ConstantRead { .. }
        | Instr::ConstantClaim { .. }
        | Instr::ConstantGet { .. }
        | Instr::ConstantAbandon { .. }
        | Instr::Jump { .. }
        | Instr::Trap { .. }
        | Instr::EnterRegion { .. }
        | Instr::LeaveRegion
        | Instr::CatchTarget { .. }
        | Instr::PushExit { .. } => {}
        Instr::Num32Unary { src, .. }
        | Instr::ConvertChecked { src, .. }
        | Instr::BoxValue { src, .. }
        | Instr::F64Unary { src, .. }
        | Instr::I64ToF64 { src, .. }
        | Instr::F64ToI64 { src, .. }
        | Instr::Move { src, .. } => all(&[*src]),
        Instr::Num32Binary { lhs, rhs, .. }
        | Instr::Num32Checked { lhs, rhs, .. }
        | Instr::I64Binary { lhs, rhs, .. }
        | Instr::IntegerPower {
            base: lhs,
            exponent: rhs,
            ..
        }
        | Instr::I64Unchecked { lhs, rhs, .. }
        | Instr::I64Bitwise { lhs, rhs, .. }
        | Instr::F64Binary { lhs, rhs, .. }
        | Instr::F64Compare { lhs, rhs, .. }
        | Instr::F64NotEqual { lhs, rhs, .. }
        | Instr::I64Compare { lhs, rhs, .. }
        | Instr::ValueEqual { lhs, rhs, .. }
        | Instr::Identical { lhs, rhs, .. }
        | Instr::EqU64 { lhs, rhs, .. }
        | Instr::AddU64 { lhs, rhs, .. }
        | Instr::AndU64 { lhs, rhs, .. }
        | Instr::StrEq { lhs, rhs, .. } => all(&[*lhs, *rhs]),
        Instr::ConstStr { .. } => {}
        Instr::ArrayNew { values, .. } => all(values),
        Instr::ArrayLen { array, .. } => all(&[*array]),
        Instr::ArrayGet { array, index, .. } => all(&[*array, *index]),
        Instr::ClosureNew { captures, .. } => all(captures),
        Instr::CallableNew {
            boxed, captures, ..
        } => {
            if let BoxedEntry::Shared(entry) = boxed {
                all(&[*entry]);
            }
            all(captures);
        }
        Instr::OpenCallable { callable, .. } => all(&[*callable]),
        Instr::StringStep {
            position, string, ..
        } => all(&[*position, *string]),
        Instr::CaptureGet { env, .. } => all(&[*env]),
        Instr::CellNew { value, .. } | Instr::ConstantSet { value, .. } => all(&[*value]),
        Instr::CellGet { cell, .. } => all(&[*cell]),
        Instr::CellSet { cell, value, .. } => all(&[*cell, *value]),
        Instr::FloatCall { args, .. }
        | Instr::IntegerCall { args, .. }
        | Instr::TextCall { args, .. }
        | Instr::Call { args, .. }
        | Instr::SelectMethod { args, .. }
        | Instr::LinkedSend { args, .. }
        | Instr::RuntimeCall { args, .. }
        | Instr::HostCall { args, .. }
        | Instr::CheckedRuntimeCall { args, .. } => all(args),
        Instr::GuardedSend { keys, args, .. } => {
            all(keys);
            all(args);
        }
        Instr::CallIndirect { callee, args, .. } => {
            all(&[*callee]);
            all(args);
        }
        Instr::MakeObject { fields, .. } => all(fields),
        Instr::VectorLiteral { elements, .. } => all(elements),
        Instr::ReadSlot { receiver, .. } => all(&[*receiver]),
        Instr::WriteSlot {
            receiver, value, ..
        } => all(&[*receiver, *value]),
        Instr::ClassOf { object, .. } => all(&[*object]),
        Instr::UnboxValue { value, .. }
        | Instr::InstanceOf { value, .. }
        | Instr::CastObject { value, .. }
        | Instr::CastClassObject { value, .. }
        | Instr::UnboxObject { value, .. } => all(&[*value]),
        Instr::Branch { cond, .. } => all(&[*cond]),
        Instr::Return { values, .. } | Instr::SignalTypeError { values, .. } => all(values),
        Instr::PushHandler { guard, .. } => {
            if let Some(guard) = guard {
                all(&[*guard]);
            }
        }
        Instr::PopBinding { token, .. } | Instr::CheckExit { token, .. } => all(&[*token]),
        Instr::NoApplicableMethod { arguments, .. } => all(&[*arguments]),
        Instr::Signal { condition, .. } => all(&[*condition]),
        Instr::Throw { payload, .. } => all(payload),
        Instr::Rethrow { exception, .. } => all(&[*exception]),
    }
}

/// Calls `f` with every register `instr` writes.
fn for_each_write(instr: &Instr, mut f: impl FnMut(Reg)) {
    let mut all = |regs: &[Reg]| regs.iter().copied().for_each(&mut f);
    match instr {
        Instr::Label { .. }
        | Instr::CellSet { .. }
        | Instr::ConstantSet { .. }
        | Instr::ConstantAbandon { .. }
        | Instr::Branch { .. }
        | Instr::Jump { .. }
        | Instr::Return { .. }
        | Instr::Trap { .. }
        | Instr::PopBinding { .. }
        | Instr::EnterRegion { .. }
        | Instr::LeaveRegion
        | Instr::CheckExit { .. }
        | Instr::Throw { .. }
        | Instr::Rethrow { .. } => {}
        Instr::ConstI64 { dst, .. }
        | Instr::ConstF64 { dst, .. }
        | Instr::Const32 { dst, .. }
        | Instr::Num32Unary { dst, .. }
        | Instr::Num32Binary { dst, .. }
        | Instr::ConstBool { dst, .. }
        | Instr::StringLiteral { dst, .. }
        | Instr::SymbolLiteral { dst, .. }
        | Instr::ConstU64 { dst, .. }
        | Instr::ConstClass { dst, .. }
        | Instr::ConstFunction { dst, .. }
        | Instr::ClosureNew { dst, .. }
        | Instr::CallableNew { dst, .. }
        | Instr::CaptureGet { dst, .. }
        | Instr::CellNew { dst, .. }
        | Instr::CellGet { dst, .. }
        | Instr::BoxValue { dst, .. }
        | Instr::I64Unchecked { dst, .. }
        | Instr::I64Bitwise { dst, .. }
        | Instr::F64Binary { dst, .. }
        | Instr::F64Unary { dst, .. }
        | Instr::F64Compare { dst, .. }
        | Instr::FloatCall { dst, .. }
        | Instr::IntegerCall { dst, .. }
        | Instr::TextCall { dst, .. }
        | Instr::F64NotEqual { dst, .. }
        | Instr::I64ToF64 { dst, .. }
        | Instr::F64ToI64 { dst, .. }
        | Instr::I64Compare { dst, .. }
        | Instr::ValueEqual { dst, .. }
        | Instr::ConstantClaim { dst, .. }
        | Instr::ConstantGet { dst, .. }
        | Instr::Identical { dst, .. }
        | Instr::SelectMethod { dst, .. }
        | Instr::MakeObject { dst, .. }
        | Instr::VectorLiteral { dst, .. }
        | Instr::RuntimeCall { dst, .. }
        | Instr::HostCall { dst, .. }
        | Instr::ReadSlot { dst, .. }
        | Instr::WriteSlot { dst, .. }
        | Instr::ClassOf { dst, .. }
        | Instr::UnboxValue { dst, .. }
        | Instr::InstanceOf { dst, .. }
        | Instr::CastObject { dst, .. }
        | Instr::CastClassObject { dst, .. }
        | Instr::EqU64 { dst, .. }
        | Instr::AddU64 { dst, .. }
        | Instr::AndU64 { dst, .. }
        | Instr::StrEq { dst, .. }
        | Instr::ConstStr { dst, .. }
        | Instr::ArrayNew { dst, .. }
        | Instr::ArrayLen { dst, .. }
        | Instr::ArrayGet { dst, .. }
        | Instr::Move { dst, .. }
        | Instr::SignalTypeError { dst, .. }
        | Instr::PushHandler { dst, .. }
        | Instr::CheckedRuntimeCall { dst, .. }
        | Instr::UnboxObject { dst, .. }
        | Instr::NoApplicableMethod { dst, .. }
        | Instr::PushExit { dst, .. } => all(&[*dst]),
        Instr::Num32Checked { dst, overflow, .. }
        | Instr::I64Binary { dst, overflow, .. }
        | Instr::IntegerPower { dst, overflow, .. } => {
            all(&[*dst, *overflow]);
        }
        Instr::ConvertChecked { dst, inexact, .. } => all(&[*dst, *inexact]),
        Instr::StringStep { dst, position, .. } => all(&[*dst, *position]),
        Instr::OpenCallable {
            typed,
            boxed,
            shape,
            ..
        } => {
            for reg in [typed, boxed, shape].into_iter().flatten() {
                all(&[*reg]);
            }
        }
        Instr::ConstantRead { ready, value, .. } => {
            all(&[*ready]);
            if let Some(value) = value {
                all(&[*value]);
            }
        }
        Instr::Call { dsts, .. }
        | Instr::LinkedSend { dsts, .. }
        | Instr::GuardedSend { dsts, .. }
        | Instr::CallIndirect { dsts, .. }
        | Instr::Signal { dsts, .. } => all(dsts),
        Instr::CatchTarget { params, .. } => all(params),
    }
}

/// Calls `f` with every label `instr` can transfer control to. A label's
/// own definition ([`Instr::Label`], [`Instr::CatchTarget`]) is not a
/// reference.
fn for_each_label_ref(instr: &Instr, mut f: impl FnMut(LabelId)) {
    match instr {
        Instr::Branch {
            then_label,
            else_label,
            ..
        } => {
            f(*then_label);
            f(*else_label);
        }
        Instr::Jump { label, .. } => f(*label),
        Instr::SignalTypeError { resume, .. } => f(*resume),
        Instr::CheckedRuntimeCall { failed, .. } => f(*failed),
        Instr::UnboxObject { otherwise, .. } => f(*otherwise),
        Instr::EnterRegion { catches } => {
            for catch in catches {
                match catch {
                    Catch::Tag { target, .. } | Catch::All { target } => f(*target),
                }
            }
        }
        // Label definitions are not references, and nothing else transfers
        // control to a label.
        Instr::Label { .. }
        | Instr::ConstI64 { .. }
        | Instr::ConstF64 { .. }
        | Instr::Const32 { .. }
        | Instr::Num32Unary { .. }
        | Instr::Num32Binary { .. }
        | Instr::Num32Checked { .. }
        | Instr::ConvertChecked { .. }
        | Instr::ConstBool { .. }
        | Instr::StringLiteral { .. }
        | Instr::SymbolLiteral { .. }
        | Instr::ConstU64 { .. }
        | Instr::ConstClass { .. }
        | Instr::ConstFunction { .. }
        | Instr::ClosureNew { .. }
        | Instr::CallableNew { .. }
        | Instr::OpenCallable { .. }
        | Instr::CaptureGet { .. }
        | Instr::CellNew { .. }
        | Instr::CellGet { .. }
        | Instr::CellSet { .. }
        | Instr::BoxValue { .. }
        | Instr::I64Binary { .. }
        | Instr::IntegerPower { .. }
        | Instr::I64Unchecked { .. }
        | Instr::I64Bitwise { .. }
        | Instr::F64Binary { .. }
        | Instr::F64Unary { .. }
        | Instr::F64Compare { .. }
        | Instr::FloatCall { .. }
        | Instr::IntegerCall { .. }
        | Instr::TextCall { .. }
        | Instr::StringStep { .. }
        | Instr::F64NotEqual { .. }
        | Instr::I64ToF64 { .. }
        | Instr::F64ToI64 { .. }
        | Instr::I64Compare { .. }
        | Instr::ValueEqual { .. }
        | Instr::ConstantRead { .. }
        | Instr::ConstantClaim { .. }
        | Instr::ConstantGet { .. }
        | Instr::ConstantSet { .. }
        | Instr::ConstantAbandon { .. }
        | Instr::Identical { .. }
        | Instr::Call { .. }
        | Instr::SelectMethod { .. }
        | Instr::LinkedSend { .. }
        | Instr::GuardedSend { .. }
        | Instr::CallIndirect { .. }
        | Instr::MakeObject { .. }
        | Instr::VectorLiteral { .. }
        | Instr::RuntimeCall { .. }
        | Instr::HostCall { .. }
        | Instr::ReadSlot { .. }
        | Instr::WriteSlot { .. }
        | Instr::ClassOf { .. }
        | Instr::UnboxValue { .. }
        | Instr::InstanceOf { .. }
        | Instr::CastObject { .. }
        | Instr::CastClassObject { .. }
        | Instr::EqU64 { .. }
        | Instr::AddU64 { .. }
        | Instr::AndU64 { .. }
        | Instr::StrEq { .. }
        | Instr::ConstStr { .. }
        | Instr::ArrayNew { .. }
        | Instr::ArrayLen { .. }
        | Instr::ArrayGet { .. }
        | Instr::Move { .. }
        | Instr::Return { .. }
        | Instr::Trap { .. }
        | Instr::PushHandler { .. }
        | Instr::PopBinding { .. }
        | Instr::NoApplicableMethod { .. }
        | Instr::Signal { .. }
        | Instr::LeaveRegion
        | Instr::CatchTarget { .. }
        | Instr::PushExit { .. }
        | Instr::CheckExit { .. }
        | Instr::Throw { .. }
        | Instr::Rethrow { .. } => {}
    }
}

/// The label `instr` defines, if any.
fn defined_label(instr: &Instr) -> Option<LabelId> {
    match instr {
        Instr::Label { label } | Instr::CatchTarget { label, .. } => Some(*label),
        _ => None,
    }
}

/// Points every branch and jump whose target only jumps on at the final
/// target instead.
fn thread_jumps(instructions: &mut [Instr]) {
    let forwards: BTreeMap<u32, LabelId> = instructions
        .windows(2)
        .filter_map(|pair| match pair {
            [Instr::Label { label }, Instr::Jump { label: next, .. }] if label != next => {
                Some((label.0, *next))
            }
            _ => None,
        })
        .collect();
    if forwards.is_empty() {
        return;
    }
    // Follow chains, stopping at a cycle of empty blocks.
    let resolve = |mut label: LabelId| {
        for _ in 0..=forwards.len() {
            match forwards.get(&label.0) {
                Some(next) => label = *next,
                None => break,
            }
        }
        label
    };
    for instr in instructions {
        match instr {
            Instr::Jump { label, .. } => *label = resolve(*label),
            Instr::Branch {
                then_label,
                else_label,
                ..
            } => {
                *then_label = resolve(*then_label);
                *else_label = resolve(*else_label);
            }
            _ => {}
        }
    }
}

/// Whether `instr` reads every operand before it writes its one result, and
/// writes that result only when it completes normally, so the result may be
/// any register: an operand's, or a local's that code after a signal it
/// leaves by exiting may still read.
///
/// Each is one tape instruction, or a host call, whose results are written
/// only when it returns. A text call copies its checked operand before it
/// can be replaced, and its tape decode of a position writes the result only
/// on the edge that found a character. A decode of a character writes its
/// result even when it finds none, before the host call that then signals,
/// so `s[p]` and `element(s, p, default:)` are left out.
fn writes_after_reads(instr: &Instr) -> bool {
    if let Instr::TextCall { function, .. } = instr {
        return !matches!(
            function,
            sem::TextFunction::StringElement | sem::TextFunction::StringElementOr
        );
    }
    matches!(
        instr,
        Instr::ConstI64 { .. }
            | Instr::ConstF64 { .. }
            | Instr::Const32 { .. }
            | Instr::ConstBool { .. }
            | Instr::ConstU64 { .. }
            | Instr::Num32Unary { .. }
            | Instr::Num32Binary { .. }
            | Instr::I64Unchecked { .. }
            | Instr::I64Bitwise { .. }
            | Instr::F64Binary { .. }
            | Instr::F64Unary { .. }
            | Instr::F64Compare { .. }
            | Instr::F64NotEqual { .. }
            | Instr::I64ToF64 { .. }
            | Instr::F64ToI64 { .. }
            | Instr::I64Compare { .. }
            | Instr::EqU64 { .. }
            | Instr::Identical { .. }
            | Instr::Move { .. }
            | Instr::FloatCall { .. }
    )
}

/// The one result register of an instruction [`writes_after_reads`]
/// accepts.
fn result_mut(instr: &mut Instr) -> Option<&mut Reg> {
    match instr {
        Instr::ConstI64 { dst, .. }
        | Instr::ConstF64 { dst, .. }
        | Instr::Const32 { dst, .. }
        | Instr::ConstBool { dst, .. }
        | Instr::ConstU64 { dst, .. }
        | Instr::Num32Unary { dst, .. }
        | Instr::Num32Binary { dst, .. }
        | Instr::I64Unchecked { dst, .. }
        | Instr::I64Bitwise { dst, .. }
        | Instr::F64Binary { dst, .. }
        | Instr::F64Unary { dst, .. }
        | Instr::F64Compare { dst, .. }
        | Instr::F64NotEqual { dst, .. }
        | Instr::I64ToF64 { dst, .. }
        | Instr::F64ToI64 { dst, .. }
        | Instr::I64Compare { dst, .. }
        | Instr::EqU64 { dst, .. }
        | Instr::AddU64 { dst, .. }
        | Instr::AndU64 { dst, .. }
        | Instr::StrEq { dst, .. }
        | Instr::ConstStr { dst, .. }
        | Instr::ArrayNew { dst, .. }
        | Instr::ArrayLen { dst, .. }
        | Instr::ArrayGet { dst, .. }
        | Instr::Identical { dst, .. }
        | Instr::Move { dst, .. }
        | Instr::FloatCall { dst, .. }
        | Instr::TextCall { dst, .. } => Some(dst),
        _ => None,
    }
}

/// Whether a register of `kind` holds a plain value, which only its
/// instructions give meaning. Callable and control registers carry facts
/// the backend tracks by register, so their moves stay.
fn plain_value(kind: ValueKind) -> bool {
    matches!(
        kind,
        ValueKind::Any
            | ValueKind::I64
            | ValueKind::F64
            | ValueKind::I32
            | ValueKind::U32
            | ValueKind::F32
            | ValueKind::U64
            | ValueKind::Bool
            | ValueKind::Char
            | ValueKind::Position
            | ValueKind::Object(_)
    )
}

/// How many times each register is read and written across `function`,
/// parameters counting as written on entry.
fn register_counts(function: &Function) -> (BTreeMap<Reg, usize>, BTreeMap<Reg, usize>) {
    let (mut reads, mut writes) = (BTreeMap::new(), BTreeMap::new());
    for param in &function.params {
        *writes.entry(param.reg).or_insert(0) += 1;
    }
    for instr in &function.instructions {
        for_each_read(instr, |reg| *reads.entry(reg).or_insert(0) += 1);
        for_each_write(instr, |reg| *writes.entry(reg).or_insert(0) += 1);
    }
    (reads, writes)
}

/// Rewrites `u = unbox b` as `u = mov v` where `b = box v` comes before it
/// in the same straight run of instructions, with `v`'s kind, and nothing
/// between them writes `v` or `b` or transfers control; then drops each box
/// nothing reads that no source local's register is. A local the compiler
/// hides, whose name has a space, is no source local.
fn forward_unboxes(function: &mut Function) {
    let mut index = 0;
    while index < function.instructions.len() {
        let Instr::BoxValue { dst, src, from, .. } = function.instructions[index] else {
            index += 1;
            continue;
        };
        for later in index + 1..function.instructions.len() {
            let instr = &function.instructions[later];
            if let Instr::UnboxValue {
                dst: unboxed,
                value,
                kind,
                span,
                ..
            } = *instr
                && value == dst
                && kind == from
            {
                function.instructions[later] = Instr::Move {
                    dst: unboxed,
                    src,
                    span,
                };
                continue;
            }
            let mut stop = matches!(instr, Instr::Label { .. } | Instr::CatchTarget { .. });
            for_each_label_ref(instr, |_| stop = true);
            for_each_write(instr, |reg| stop |= reg == src || reg == dst);
            if stop {
                break;
            }
        }
        index += 1;
    }
    let (reads, _) = register_counts(function);
    let named: BTreeSet<Reg> = function
        .locals
        .iter()
        .filter(|local| !local.name.contains(' '))
        .map(|local| local.reg)
        .chain(function.params.iter().map(|param| param.reg))
        .collect();
    function.instructions.retain(|instr| {
        !matches!(instr, Instr::BoxValue { dst, .. }
            if !reads.contains_key(dst) && !named.contains(dst))
    });
}

/// Rewrites `t = op(...); d = mov t` as `d = op(...)` where that is the
/// only write and the only read of `t`.
fn coalesce_moves(function: &mut Function) {
    let (reads, writes) = register_counts(function);
    let named: BTreeSet<Reg> = function
        .locals
        .iter()
        .map(|local| local.reg)
        .chain(function.params.iter().map(|param| param.reg))
        .collect();
    let once = |counts: &BTreeMap<Reg, usize>, reg: Reg| counts.get(&reg) == Some(&1);
    let mut index = 0;
    while index + 1 < function.instructions.len() {
        let coalesce = match (
            &function.instructions[index],
            &function.instructions[index + 1],
        ) {
            (producer, Instr::Move { dst, src, .. }) if writes_after_reads(producer) => {
                let mut produced = None;
                for_each_write(producer, |reg| produced = Some(reg));
                produced == Some(*src)
                    && src != dst
                    && once(&writes, *src)
                    && once(&reads, *src)
                    && !named.contains(src)
                    && function.register_kind(*src) == function.register_kind(*dst)
                    && function.register_kind(*src).is_some_and(plain_value)
            }
            _ => false,
        };
        if coalesce {
            let Instr::Move { dst, .. } = function.instructions.remove(index + 1) else {
                unreachable!("the next instruction is the move");
            };
            if let Some(result) = result_mut(&mut function.instructions[index]) {
                *result = dst;
            }
            // `t = mov d; d = mov t` leaves `d = mov d`, which does nothing.
            if matches!(function.instructions[index], Instr::Move { dst, src, .. } if dst == src) {
                function.instructions.remove(index);
                continue;
            }
        }
        index += 1;
    }
}

/// Pairs each character read with the first `next-position` of the same
/// string and position registers that may use its decode.
///
/// The `next-position` may when every way to reach it passes the read with
/// `s` and `p` unchanged since. In layout order the read comes first; no
/// instruction between them writes `s` or `p`; and no label between them,
/// the `next-position`'s own block included, is referred to from outside
/// the stretch from the read to the `next-position`, so control enters that
/// stretch only through the read. The read's decode stays valid however
/// often the stretch loops within itself, as `s` and `p` do not change.
///
/// Exceptional control counts as a reference too: a region's catch targets
/// are labels its `EnterRegion` names. That relies on regions being lexical,
/// as lowering emits them, so a jump never lands in the middle of one.
fn share_decodes(instructions: &mut [Instr]) {
    let positions: BTreeMap<u32, usize> = instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instr)| defined_label(instr).map(|label| (label.0, index)))
        .collect();
    let mut pairs = Vec::new();
    for (read, instr) in instructions.iter().enumerate() {
        let Instr::TextCall {
            function: sem::TextFunction::StringElement | sem::TextFunction::StringElementOr,
            args,
            share: None,
            ..
        } = instr
        else {
            continue;
        };
        let (string, position) = (args[0], args[1]);
        let Some(next) = (read + 1..instructions.len()).find_map(|index| {
            let candidate = &instructions[index];
            if let Instr::TextCall {
                function: sem::TextFunction::NextPosition,
                args,
                share: None,
                ..
            } = candidate
                && args.as_slice() == [string, position]
            {
                return Some(Some(index));
            }
            let mut writes = false;
            for_each_write(candidate, |reg| writes |= reg == string || reg == position);
            // A write ends the search: no later `next-position` may share.
            writes.then_some(None)
        }) else {
            continue;
        };
        let Some(next) = next else {
            continue;
        };
        let stretch = read..=next;
        let entered_from_outside = instructions.iter().enumerate().any(|(index, instr)| {
            if stretch.contains(&index) {
                return false;
            }
            let mut enters = false;
            for_each_label_ref(instr, |label| {
                enters |= positions
                    .get(&label.0)
                    .is_some_and(|at| *at > read && *at <= next);
            });
            enters
        });
        if !entered_from_outside && !pairs.iter().any(|(_, used)| *used == next) {
            pairs.push((read, next));
        }
    }
    for (slot, (read, next)) in pairs.into_iter().enumerate() {
        let slot = u32::try_from(slot).unwrap_or(u32::MAX);
        for (index, share) in [
            (read, DecodeShare::Keeps(slot)),
            (next, DecodeShare::Uses(slot)),
        ] {
            if let Instr::TextCall { share: field, .. } = &mut instructions[index] {
                *field = Some(share);
            }
        }
    }
}

/// Moves constants made inside a loop to just before it.
fn hoist_loop_constants(function: &mut Function) {
    // Hoisting out of an inner loop may expose the constant to an outer
    // loop, so repeat until nothing moves; each round moves at least one
    // instruction earlier, and nesting bounds the rounds.
    for _ in 0..function.instructions.len() {
        if !hoist_once(function) {
            break;
        }
    }
}

/// Hoists the constants of the first loop that has any, and returns
/// whether it did.
fn hoist_once(function: &mut Function) -> bool {
    let (_, writes) = register_counts(function);
    let instructions = &function.instructions;
    let positions: BTreeMap<u32, usize> = instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instr)| defined_label(instr).map(|label| (label.0, index)))
        .collect();
    for (end, instr) in instructions.iter().enumerate() {
        let Instr::Jump { label: header, .. } = instr else {
            continue;
        };
        let Some(&start) = positions.get(&header.0) else {
            continue;
        };
        // A backward jump closes a loop `start..=end`, entered by the jump
        // just before its header.
        if start >= end
            || start == 0
            || !matches!(instructions[start - 1], Instr::Jump { label, .. } if label == *header)
        {
            continue;
        }
        let inside = start..=end;
        let enters_from_outside = instructions.iter().enumerate().any(|(index, instr)| {
            if inside.contains(&index) || index == start - 1 {
                return false;
            }
            let mut enters = false;
            for_each_label_ref(instr, |label| {
                enters |= positions
                    .get(&label.0)
                    .is_some_and(|position| inside.contains(position));
            });
            enters
        });
        if enters_from_outside {
            continue;
        }
        let hoisted: Vec<usize> = (start..end)
            .filter(|&index| match &instructions[index] {
                Instr::ConstI64 { dst, .. }
                | Instr::ConstF64 { dst, .. }
                | Instr::Const32 { dst, .. }
                | Instr::ConstBool { dst, .. }
                | Instr::ConstU64 { dst, .. } => writes.get(dst) == Some(&1),
                _ => false,
            })
            .collect();
        if hoisted.is_empty() {
            continue;
        }
        let mut moved = Vec::with_capacity(hoisted.len());
        for &index in hoisted.iter().rev() {
            moved.push(function.instructions.remove(index));
        }
        moved.reverse();
        let entry = start - 1;
        function.instructions.splice(entry..entry, moved);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::{FunctionKind, Local, Param};
    use crate::sem;
    use crate::syntax::Span;
    use alloc::vec;

    const SPAN: Span = Span { start: 0, end: 0 };

    fn reg(n: u32) -> Reg {
        Reg(n)
    }

    fn function(registers: u32, instructions: Vec<Instr>) -> Function {
        Function {
            id: super::super::FunctionId(0),
            name: "f".into(),
            kind: FunctionKind::Function(sem::FunctionId(0)),
            params: vec![Param {
                source: None,
                name: "x".into(),
                kind: ValueKind::I64,
                reg: reg(1),
            }],
            ret: ValueKind::I64,
            rets: vec![ValueKind::I64],
            register_kinds: vec![ValueKind::I64; registers as usize],
            locals: vec![Local {
                source: sem::LocalId(0),
                name: "i".into(),
                kind: ValueKind::I64,
                reg: reg(2),
            }],
            instructions,
            span: SPAN,
        }
    }

    fn label(n: u32) -> LabelId {
        LabelId(n)
    }

    #[test]
    fn jumps_go_past_blocks_that_only_jump() {
        let mut instructions = vec![
            Instr::Branch {
                cond: reg(1),
                then_label: label(1),
                else_label: label(2),
                span: SPAN,
            },
            Instr::Label { label: label(1) },
            Instr::Jump {
                label: label(2),
                span: SPAN,
            },
            Instr::Label { label: label(2) },
            Instr::Jump {
                label: label(3),
                span: SPAN,
            },
            Instr::Label { label: label(3) },
            Instr::Return {
                values: vec![reg(1)],
                span: SPAN,
            },
        ];
        thread_jumps(&mut instructions);
        assert_eq!(
            instructions[0],
            Instr::Branch {
                cond: reg(1),
                then_label: label(3),
                else_label: label(3),
                span: SPAN,
            }
        );
        assert_eq!(
            instructions[2],
            Instr::Jump {
                label: label(3),
                span: SPAN
            }
        );
    }

    #[test]
    fn a_cycle_of_empty_blocks_stops() {
        let mut instructions = vec![
            Instr::Label { label: label(1) },
            Instr::Jump {
                label: label(2),
                span: SPAN,
            },
            Instr::Label { label: label(2) },
            Instr::Jump {
                label: label(1),
                span: SPAN,
            },
        ];
        thread_jumps(&mut instructions);
        assert!(matches!(instructions[1], Instr::Jump { .. }));
    }

    #[test]
    fn a_result_moved_to_a_local_is_written_there() {
        // r3 = r2 + r1; r2 = mov r3  =>  r2 = r2 + r1
        let mut f = function(
            3,
            vec![
                Instr::I64Unchecked {
                    dst: reg(3),
                    op: super::super::I64ArithOp::Add,
                    lhs: reg(2),
                    rhs: reg(1),
                    span: SPAN,
                },
                Instr::Move {
                    dst: reg(2),
                    src: reg(3),
                    span: SPAN,
                },
                Instr::Return {
                    values: vec![reg(2)],
                    span: SPAN,
                },
            ],
        );
        coalesce_moves(&mut f);
        assert_eq!(f.instructions.len(), 2);
        assert!(matches!(f.instructions[0], Instr::I64Unchecked { dst, .. } if dst == reg(2)));
    }

    #[test]
    fn a_temporary_read_again_or_checked_is_kept() {
        // The temporary is read after the move, so it must keep its value.
        let read_again = vec![
            Instr::I64Unchecked {
                dst: reg(3),
                op: super::super::I64ArithOp::Add,
                lhs: reg(2),
                rhs: reg(1),
                span: SPAN,
            },
            Instr::Move {
                dst: reg(2),
                src: reg(3),
                span: SPAN,
            },
            Instr::Return {
                values: vec![reg(3)],
                span: SPAN,
            },
        ];
        // An overflow-checked add reports its operands after writing its
        // result, so its result cannot be an operand.
        let checked = vec![
            Instr::I64Binary {
                dst: reg(3),
                overflow: reg(4),
                op: super::super::I64ArithOp::Add,
                lhs: reg(2),
                rhs: reg(1),
                span: SPAN,
            },
            Instr::Move {
                dst: reg(2),
                src: reg(3),
                span: SPAN,
            },
            Instr::Return {
                values: vec![reg(2), reg(4)],
                span: SPAN,
            },
        ];
        for instructions in [read_again, checked] {
            let mut f = function(4, instructions.clone());
            coalesce_moves(&mut f);
            assert_eq!(f.instructions, instructions);
        }
    }

    fn counting_loop() -> Vec<Instr> {
        // i = 0; while i < 10: i = i + 1
        vec![
            Instr::ConstI64 {
                dst: reg(2),
                value: 0,
                span: SPAN,
            },
            Instr::Jump {
                label: label(1),
                span: SPAN,
            },
            Instr::Label { label: label(1) },
            Instr::ConstI64 {
                dst: reg(3),
                value: 10,
                span: SPAN,
            },
            Instr::I64Compare {
                dst: reg(4),
                op: sem::CompareOp::Lt,
                lhs: reg(2),
                rhs: reg(3),
                span: SPAN,
            },
            Instr::Branch {
                cond: reg(4),
                then_label: label(2),
                else_label: label(3),
                span: SPAN,
            },
            Instr::Label { label: label(2) },
            Instr::ConstI64 {
                dst: reg(5),
                value: 1,
                span: SPAN,
            },
            Instr::I64Unchecked {
                dst: reg(2),
                op: super::super::I64ArithOp::Add,
                lhs: reg(2),
                rhs: reg(5),
                span: SPAN,
            },
            Instr::Jump {
                label: label(1),
                span: SPAN,
            },
            Instr::Label { label: label(3) },
            Instr::Return {
                values: vec![reg(2)],
                span: SPAN,
            },
        ]
    }

    #[test]
    fn loop_constants_are_made_once_before_the_loop() {
        let mut f = function(5, counting_loop());
        hoist_loop_constants(&mut f);
        // Both loop constants now precede the jump into the loop.
        assert!(matches!(
            f.instructions[1],
            Instr::ConstI64 { value: 10, .. }
        ));
        assert!(matches!(
            f.instructions[2],
            Instr::ConstI64 { value: 1, .. }
        ));
        assert_eq!(
            f.instructions[3],
            Instr::Jump {
                label: label(1),
                span: SPAN
            }
        );
        assert_eq!(f.instructions.len(), counting_loop().len());
    }

    #[test]
    fn a_constant_written_twice_or_a_loop_entered_twice_stays() {
        // The `1` register is also written after the loop.
        let mut written_twice = counting_loop();
        written_twice.push(Instr::ConstI64 {
            dst: reg(5),
            value: 2,
            span: SPAN,
        });
        // A branch before the loop enters its body directly.
        let mut entered_twice = counting_loop();
        entered_twice.insert(
            0,
            Instr::Branch {
                cond: reg(1),
                then_label: label(2),
                else_label: label(3),
                span: SPAN,
            },
        );
        for (instructions, kept) in [(written_twice, 10), (entered_twice, 0)] {
            let mut f = function(5, instructions);
            hoist_loop_constants(&mut f);
            let in_loop: Vec<i64> = f
                .instructions
                .iter()
                .skip_while(
                    |instr| !matches!(instr, Instr::Label { label } if *label == LabelId(1)),
                )
                .filter_map(|instr| match instr {
                    Instr::ConstI64 { value, .. } => Some(*value),
                    _ => None,
                })
                .collect();
            assert!(in_loop.contains(&1), "{in_loop:?}");
            let _ = kept;
        }
    }

    fn text_call(dst: u32, function: sem::TextFunction) -> Instr {
        Instr::TextCall {
            dst: reg(dst),
            function,
            args: vec![reg(1), reg(2)],
            check: None,
            share: None,
            span: SPAN,
        }
    }

    fn shares(instructions: &[Instr]) -> Vec<Option<DecodeShare>> {
        instructions
            .iter()
            .filter_map(|instr| match instr {
                Instr::TextCall { share, .. } => Some(*share),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_next_position_reached_only_through_the_read_shares_its_decode() {
        let pair = || {
            vec![
                text_call(3, sem::TextFunction::StringElement),
                Instr::Jump {
                    label: label(1),
                    span: SPAN,
                },
                Instr::Label { label: label(1) },
                text_call(4, sem::TextFunction::NextPosition),
            ]
        };
        let mut shared = pair();
        share_decodes(&mut shared);
        assert_eq!(
            shares(&shared),
            [Some(DecodeShare::Keeps(0)), Some(DecodeShare::Uses(0))]
        );

        // A branch before the read reaches the `next-position` past it.
        let mut entered = pair();
        entered.insert(
            0,
            Instr::Branch {
                cond: reg(5),
                then_label: label(1),
                else_label: label(1),
                span: SPAN,
            },
        );
        share_decodes(&mut entered);
        assert_eq!(shares(&entered), [None, None]);

        // A write to the position between them.
        let mut written = pair();
        written.insert(
            1,
            Instr::ConstI64 {
                dst: reg(2),
                value: 0,
                span: SPAN,
            },
        );
        share_decodes(&mut written);
        assert_eq!(shares(&written), [None, None]);

        // A region opened before the read catches into the stretch.
        let mut caught = pair();
        caught.insert(
            0,
            Instr::EnterRegion {
                catches: vec![Catch::All { target: label(1) }],
            },
        );
        share_decodes(&mut caught);
        assert_eq!(shares(&caught), [None, None]);
    }

    #[test]
    fn a_loop_inside_the_stretch_keeps_the_pair() {
        // The read, then a loop that jumps back within the stretch, then the
        // `next-position`: every way in passes the read.
        let mut instructions = vec![
            text_call(3, sem::TextFunction::StringElement),
            Instr::Jump {
                label: label(1),
                span: SPAN,
            },
            Instr::Label { label: label(1) },
            Instr::Branch {
                cond: reg(5),
                then_label: label(1),
                else_label: label(2),
                span: SPAN,
            },
            Instr::Label { label: label(2) },
            text_call(4, sem::TextFunction::NextPosition),
        ];
        share_decodes(&mut instructions);
        assert_eq!(
            shares(&instructions),
            [Some(DecodeShare::Keeps(0)), Some(DecodeShare::Uses(0))]
        );
    }
}
