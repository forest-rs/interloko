//! Compatibility re-exports and semantic-to-runtime layout construction.
//!
//! Executable runtime host support now lives in [`interloko_runtime`]. This
//! module keeps the historical `cairn_compiler::runtime` path available while
//! the compiler owns the one part that still depends on semantic IR: converting
//! a resolved [`sem::Module`] into a runtime layout.

pub use interloko_runtime::*;

use interloko_runtime::{
    HostObjectKind as RtHostObjectKind, RuntimeClass as RtRuntimeClass,
    RuntimeGeneric as RtRuntimeGeneric, RuntimeLayout as RtRuntimeLayout,
    RuntimeMethod as RtRuntimeMethod, RuntimeSlot as RtRuntimeSlot, ValueKind as RuntimeValueKind,
};

use crate::sem;

/// Creates a runtime layout from a semantic module.
#[must_use]
pub fn runtime_layout_from_sem(module: &sem::Module) -> RtRuntimeLayout {
    RtRuntimeLayout {
        classes: module
            .classes
            .iter()
            .map(|class| RtRuntimeClass {
                name: class.name.clone(),
                kind: match class.origin {
                    sem::ClassOrigin::Native => RtHostObjectKind::Native,
                    sem::ClassOrigin::BuiltinValue { kind } => {
                        RtHostObjectKind::BuiltinValue { kind }
                    }
                    sem::ClassOrigin::HostBacked { .. } => RtHostObjectKind::HostBacked,
                },
                superclass: class.superclass,
                slots: class
                    .effective_slots
                    .iter()
                    .map(|slot| RtRuntimeSlot {
                        name: slot.name.clone(),
                        type_name: slot.type_name.clone(),
                        kind: runtime_value_kind(slot.kind).unwrap_or(RuntimeValueKind::Any),
                        source_class: slot.source_class,
                        source_slot: slot.source_slot,
                        init_keyword: slot.init_keyword.clone(),
                        mutable: matches!(slot.mutability, sem::SlotMutability::Mutable),
                        getter: slot.accessor.name.clone(),
                        setter: setter_generic_for_slot(module, slot),
                        host_backed: matches!(slot.origin, sem::SlotOrigin::HostBacked { .. }),
                    })
                    .collect(),
            })
            .collect(),
        generics: module
            .generics
            .iter()
            .map(|generic| RtRuntimeGeneric {
                name: generic.name.clone(),
                methods: module
                    .methods
                    .iter()
                    .filter(|method| method.generic == generic.id)
                    .map(|method| method.id)
                    .collect(),
            })
            .collect(),
        methods: module
            .methods
            .iter()
            .map(|method| RtRuntimeMethod {
                generic: method.generic,
                specializers: method.specializers.clone(),
            })
            .collect(),
    }
}

/// Converts semantic value categories into runtime-representable categories.
///
/// Compile-time-only and invalid semantic categories return `None`.
#[must_use]
pub const fn runtime_value_kind(kind: sem::ValueKind) -> Option<RuntimeValueKind> {
    match kind {
        sem::ValueKind::Any => Some(RuntimeValueKind::Any),
        sem::ValueKind::I64 => Some(RuntimeValueKind::I64),
        sem::ValueKind::Bool => Some(RuntimeValueKind::Bool),
        sem::ValueKind::String(class) => Some(RuntimeValueKind::String(class)),
        sem::ValueKind::Symbol(class) => Some(RuntimeValueKind::Symbol(class)),
        sem::ValueKind::Vector(class) => Some(RuntimeValueKind::Vector(class)),
        sem::ValueKind::Object(class) => Some(RuntimeValueKind::Object(class)),
        sem::ValueKind::ClassObject(class) => Some(RuntimeValueKind::ClassObject(class)),
        sem::ValueKind::GenericFunctionObject(generic) => {
            Some(RuntimeValueKind::GenericFunctionObject(generic))
        }
        sem::ValueKind::MethodObject(method) => Some(RuntimeValueKind::MethodObject(method)),
        sem::ValueKind::SlotDescriptorObject { class, slot } => {
            Some(RuntimeValueKind::SlotDescriptorObject { class, slot })
        }
        sem::ValueKind::KnownCallable(_) | sem::ValueKind::Error => None,
    }
}

fn setter_generic_for_slot(
    module: &sem::Module,
    slot: &sem::EffectiveSlot,
) -> Option<sem::GenericFunctionId> {
    let source_effective_slot = module
        .classes
        .get(slot.source_class.0 as usize)?
        .effective_slots
        .iter()
        .find(|candidate| {
            candidate.source_class == slot.source_class && candidate.source_slot == slot.source_slot
        })?
        .id;
    module
        .generics
        .iter()
        .find(|generic| {
            generic.origin
                == sem::GenericFunctionOrigin::SlotSetter {
                    class: slot.source_class,
                    slot: source_effective_slot,
                }
        })
        .map(|generic| generic.id)
}
