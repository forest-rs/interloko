//! Which reads of an assigned local may use its register directly.
//!
//! An assigned local lives in one register that its binding and every
//! assignment write ([`super::BodyLowering::home`]). A read that uses that
//! register directly is only correct while no assignment to the local runs
//! between the read and the operation that consumes it: in `x + (x := 1)`
//! the addition must see the old `x`, so that read copies the register.
//! Every other read of an assigned local can use the register as it is,
//! which keeps a loop like `i := i + 1` free of copies.
//!
//! A read is pending until its consumer runs. For an operation (a call, an
//! arithmetic operator, a slot write, `values`...) that is after all its
//! operands, so the locals the later operands assign are hazards for the
//! earlier ones. Control forms consume their parts in order: a test
//! decides its branch before the branch runs, statements finish before the
//! next starts, and `if` and loops move their results into registers of
//! their own. A body's result, an assignment's value and a handler's arms
//! may be the register of a read, so they keep the hazards around them.
//!
//! A value bound by `let` outlives its expression; lowering copies one that
//! is an assigned local's register when it binds it. Reads this analysis
//! does not list, such as those in a constant's value lowered inline, copy.

use alloc::vec::Vec;

use crate::sem;

/// The reads of assigned locals in `block` that may use the local's
/// register directly, by address. Closure bodies are lowered on their own
/// and are not visited.
pub(super) fn direct_reads(block: &sem::Block) -> Vec<*const sem::Expr> {
    let mut out = Vec::new();
    if !block.assigned_locals().is_empty() {
        visit_block(block, &[], &mut out);
    }
    out
}

fn visit_block(block: &sem::Block, hazards: &[sem::LocalId], out: &mut Vec<*const sem::Expr>) {
    for statement in &block.statements {
        match statement {
            sem::Statement::Let(binding) => visit(&binding.value, &[], out),
            sem::Statement::Expr(expr) => visit(expr, &[], out),
        }
    }
    visit(&block.result, hazards, out);
}

fn visit(expr: &sem::Expr, hazards: &[sem::LocalId], out: &mut Vec<*const sem::Expr>) {
    match expr {
        sem::Expr::Local { local, .. } => {
            if !hazards.contains(local) {
                out.push(core::ptr::from_ref(expr));
            }
        }
        sem::Expr::If { .. } | sem::Expr::Loop { .. } => {
            expr.for_each_child(|child| visit(child, &[], out));
        }
        sem::Expr::Block { body, cleanup, .. } => {
            visit_block(body, hazards, out);
            if let Some(cleanup) = cleanup {
                visit_block(cleanup, &[], out);
            }
        }
        sem::Expr::Handle { body, restart, .. } => {
            visit(body, hazards, out);
            if let Some(guard) = &restart.guard {
                visit(guard, &[], out);
            }
            visit(&restart.body, hazards, out);
        }
        sem::Expr::AssignLocal { value, .. } | sem::Expr::AssignCapture { value, .. } => {
            visit(value, hazards, out);
        }
        _ => {
            let mut operands = Vec::new();
            expr.for_each_child(|child| operands.push(child));
            // The locals each operand's later siblings assign, innermost
            // hazards first.
            let mut later = Vec::with_capacity(operands.len());
            let mut assigned: Vec<sem::LocalId> = Vec::new();
            for operand in operands.iter().rev() {
                later.push(assigned.clone());
                for local in operand.assigned_locals() {
                    if !assigned.contains(&local) {
                        assigned.push(local);
                    }
                }
            }
            for (operand, mut operand_hazards) in operands.into_iter().zip(later.into_iter().rev())
            {
                operand_hazards.extend_from_slice(hazards);
                visit(operand, &operand_hazards, out);
            }
        }
    }
}
