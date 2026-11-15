//! The `<no-applicable-method>` conditions that failed sends signal.
//!
//! A generic send whose arguments no method accepts does not trap. Its
//! generic's dispatcher, which every send that finds no method reaches,
//! asks the driver for a `<no-applicable-method>`
//! ([`crate::control::CONDITION_NO_APPLICABLE_METHOD`]) and signals it, so
//! handlers, choosers and agents see the generic, the arguments and a stable
//! code. This module builds those conditions and reads them back for
//! reports.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use execution_tape::host::ValueRef;
use execution_tape::value::{Obj, Value};
use interloko_model::condition::{
    ConditionClass, NO_APPLICABLE_METHOD_CODE, NoApplicableMethodSlot,
};
use interloko_model::{ClassId, GenericFunctionId, SlotId};

use crate::{InterlokoHost, RuntimeError, RuntimeObjectStorage};

/// What a `<no-applicable-method>` condition reports, read from its slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoApplicableMethodReport {
    /// The stable code, [`NO_APPLICABLE_METHOD_CODE`] when the runtime
    /// signalled the condition. A program that makes its own
    /// `<no-applicable-method>` may store any text here.
    pub code: String,
    /// The generic's name, such as `area`.
    pub generic: String,
    /// The send's arguments, in order, written as Loko literal text where
    /// they have one and as `{<class>}` for an instance; see
    /// [`InterlokoHost::describe_value`].
    pub arguments: Vec<String>,
    /// The class of each argument, which is what dispatch looked at, such
    /// as `<square>` or `<integer>`.
    pub argument_classes: Vec<String>,
}

impl core::fmt::Display for NoApplicableMethodReport {
    /// Writes `dispatch.no-applicable-method: no method of area applies to
    /// (<square>), called with ({<square>})`.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "{}: no method of {} applies to ({}), called with ({})",
            self.code,
            self.generic,
            self.argument_classes.join(", "),
            self.arguments.join(", ")
        )
    }
}

impl InterlokoHost {
    /// The `<no-applicable-method>` class of this host's layout, if it has
    /// one.
    #[must_use]
    pub fn no_applicable_method_class(&self) -> Option<ClassId> {
        self.layout
            .class_id(ConditionClass::NoApplicableMethod.name())
    }

    /// Makes the `<no-applicable-method>` a send to `generic` signals when
    /// no method accepts `arguments`, a `<vector>` of the send's arguments.
    pub fn make_no_applicable_method(
        &mut self,
        generic: GenericFunctionId,
        arguments: Value,
    ) -> Result<Obj, RuntimeError> {
        let class = self
            .no_applicable_method_class()
            .ok_or(RuntimeError::MissingBuiltinClass {
                name: ConditionClass::NoApplicableMethod.name(),
            })?;
        let generic_meta = self
            .push_generic_meta(generic)
            .map_err(|_| RuntimeError::InvalidGeneric { generic })?;
        let code = self
            .push_string(NO_APPLICABLE_METHOD_CODE.to_string())
            .map_err(|_| RuntimeError::MissingBuiltinClass { name: "<string>" })?;
        let mut fields = [Value::Unit, Value::Unit, Value::Unit];
        fields[NoApplicableMethodSlot::Generic.index()] = generic_meta;
        fields[NoApplicableMethodSlot::Arguments.index()] = arguments;
        fields[NoApplicableMethodSlot::Code.index()] = code;
        let refs = fields.iter().map(ValueRef::from_value).collect::<Vec<_>>();
        match self.make_object(class, &refs)? {
            Value::Obj(obj) => Ok(obj),
            _ => Err(RuntimeError::InvalidClass { class }),
        }
    }

    /// Reads what `condition` reports, when it is a `<no-applicable-method>`
    /// (or an instance of a subclass, whose layout starts with its slots).
    #[must_use]
    pub fn no_applicable_method_report(&self, condition: Obj) -> Option<NoApplicableMethodReport> {
        let class = self.no_applicable_method_class()?;
        let object = self.object(condition).ok()?;
        if !self.layout.is_same_or_subclass(object.class, class) {
            return None;
        }
        let slot = |slot: NoApplicableMethodSlot| {
            let id = SlotId(u32::try_from(slot.index()).ok()?);
            self.read_slot(condition, id).ok()
        };
        let generic = match slot(NoApplicableMethodSlot::Generic)? {
            Value::Obj(meta) => match &self.object(meta).ok()?.storage {
                RuntimeObjectStorage::GenericMeta { generic } => self
                    .layout
                    .generic(*generic)
                    .map_or_else(|| format!("generic#{}", generic.0), |g| g.name.clone()),
                _ => return None,
            },
            _ => return None,
        };
        let elements = match slot(NoApplicableMethodSlot::Arguments)? {
            Value::Obj(vector) => match &self.object(vector).ok()?.storage {
                RuntimeObjectStorage::Vector { elements } => elements.clone(),
                _ => return None,
            },
            _ => return None,
        };
        let code = self
            .string_text(&slot(NoApplicableMethodSlot::Code)?)
            .ok()?
            .to_string();
        let class_name = |value: &Value| {
            self.value_class(value)
                .and_then(|class| self.layout.class(class))
                .map_or_else(|| "{unknown}".to_string(), |class| class.name.clone())
        };
        Some(NoApplicableMethodReport {
            code,
            generic,
            arguments: elements
                .iter()
                .map(|value| self.describe_value(ValueRef::from_value(value)))
                .collect(),
            argument_classes: elements.iter().map(class_name).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use interloko_model::OBJECT_CLASS_ID;
    use interloko_model::condition::NO_APPLICABLE_METHOD_SLOTS;

    use super::*;
    use crate::{
        HostObjectKind, RuntimeClass, RuntimeGeneric, RuntimeLayout, RuntimeMethod, RuntimeSlot,
        ValueKind,
    };

    fn class(name: &str, superclass: Option<ClassId>, slots: Vec<RuntimeSlot>) -> RuntimeClass {
        RuntimeClass {
            name: name.into(),
            kind: HostObjectKind::Native,
            superclass,
            slots,
        }
    }

    /// `<object>`, `<string>`, `<vector>`, `<generic-function>`,
    /// `<integer>`, `<square>`, then the kernel condition classes, with
    /// one generic `area`.
    fn host() -> InterlokoHost {
        let no_method = ClassId(9);
        let kinds = [
            ValueKind::Object(ClassId(3)),
            ValueKind::Vector(ClassId(2)),
            ValueKind::String(ClassId(1)),
        ];
        let slots = NO_APPLICABLE_METHOD_SLOTS
            .iter()
            .zip(kinds)
            .enumerate()
            .map(|(index, (slot, kind))| RuntimeSlot {
                name: slot.name.into(),
                type_name: slot.type_name.into(),
                kind,
                source_class: no_method,
                source_slot: SlotId(u32::try_from(index).expect("slot index")),
                init_keyword: slot.init_keyword.into(),
                mutable: false,
                getter: slot.name.into(),
                setter: None,
                host_backed: false,
            })
            .collect();
        let builtin = |name: &str, kind| RuntimeClass {
            name: name.into(),
            kind: HostObjectKind::BuiltinValue { kind },
            superclass: Some(OBJECT_CLASS_ID),
            slots: Vec::new(),
        };
        InterlokoHost::new_checked(RuntimeLayout {
            classes: vec![
                class("<object>", None, Vec::new()),
                builtin("<string>", interloko_model::BuiltinValueClass::String),
                builtin("<vector>", interloko_model::BuiltinValueClass::Vector),
                builtin(
                    "<generic-function>",
                    interloko_model::BuiltinValueClass::GenericFunction,
                ),
                builtin("<integer>", interloko_model::BuiltinValueClass::Integer),
                class("<square>", Some(OBJECT_CLASS_ID), Vec::new()),
                class("<condition>", Some(OBJECT_CLASS_ID), Vec::new()),
                class("<error>", Some(ClassId(6)), Vec::new()),
                class("<type-error>", Some(ClassId(7)), Vec::new()),
                class("<no-applicable-method>", Some(ClassId(7)), slots),
            ],
            generics: vec![RuntimeGeneric {
                name: "area".into(),
                arity: 1,
                methods: vec![interloko_model::MethodId(0)],
            }],
            methods: vec![RuntimeMethod {
                generic: GenericFunctionId(0),
                specializers: vec![Some(ClassId(5))],
            }],
        })
        .expect("layout")
    }

    #[test]
    fn a_failed_send_reports_its_generic_arguments_and_code() {
        let mut host = host();
        let square = host.make_object(ClassId(5), &[]).expect("square");
        let seven = host.box_top_value(Value::I64(7)).expect("box");
        let arguments = host.push_vector(vec![square, seven]).expect("vector");
        let condition = host
            .make_no_applicable_method(GenericFunctionId(0), arguments)
            .expect("condition");
        let report = host.no_applicable_method_report(condition).expect("report");
        assert_eq!(
            report,
            NoApplicableMethodReport {
                code: NO_APPLICABLE_METHOD_CODE.into(),
                generic: "area".into(),
                arguments: vec!["{<square>}".into(), "7".into()],
                argument_classes: vec!["<square>".into(), "<integer>".into()],
            }
        );
        assert_eq!(
            report.to_string(),
            "dispatch.no-applicable-method: no method of area applies to \
             (<square>, <integer>), called with ({<square>}, 7)"
        );
        // Another condition reports nothing.
        let Value::Obj(square) = host.make_object(ClassId(5), &[]).expect("square") else {
            panic!("square is an object");
        };
        assert_eq!(host.no_applicable_method_report(square), None);
        assert_eq!(host.type_error_report(condition), None);
    }

    #[test]
    fn an_unknown_generic_makes_no_condition() {
        let mut host = host();
        let arguments = host.push_vector(Vec::new()).expect("vector");
        assert_eq!(
            host.make_no_applicable_method(GenericFunctionId(3), arguments),
            Err(RuntimeError::InvalidGeneric {
                generic: GenericFunctionId(3)
            })
        );
    }
}
