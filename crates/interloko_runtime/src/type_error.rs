//! The `<type-error>` conditions that failed runtime checks signal.
//!
//! A check that fails at run time ([`crate::control::TypeErrorSite`]) does
//! not trap. Compiled code asks the driver for a `<type-error>`
//! ([`crate::control::CONDITION_TYPE_ERROR`]) and signals it, so handlers,
//! choosers and agents see the rejected value, the expected type, the site
//! and a stable code. This module builds those conditions and reads them
//! back for reports.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use execution_tape::host::{HostContext, HostError, ValueRef};
use execution_tape::value::{Obj, Value};
use interloko_model::condition::{ConditionClass, TypeErrorKind, TypeErrorSlot};
use interloko_model::{ClassId, FloatBits, SingleFloatBits, SlotId};

use crate::control::{CHECK_FAILED_TAG, TypeErrorSite};
use crate::{
    INTERLOKO_OBJECT_TYPE, INTERLOKO_VALUE_TYPE, InterlokoHost, RuntimeError, RuntimeObjectStorage,
};

/// What a `<type-error>` condition reports, read from its slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeErrorReport {
    /// The stable code, such as `type-error.cast`. A program that makes its
    /// own `<type-error>` may store any text here; [`Self::kind`] parses it.
    pub code: String,
    /// The printed expected type, such as `<circle>`.
    pub expected: String,
    /// Where the check is, such as `typing:def…:s2`.
    pub site: String,
    /// The rejected value, written as Loko literal text where it has one,
    /// such as `7`, `"text"` or `#[1, 2]`, and as `{<class>}` for an
    /// instance.
    pub value: String,
}

impl TypeErrorReport {
    /// The kind of check that failed, when [`Self::code`] is one of the
    /// runtime's codes.
    #[must_use]
    pub fn kind(&self) -> Option<TypeErrorKind> {
        TypeErrorKind::from_code(&self.code)
    }
}

impl core::fmt::Display for TypeErrorReport {
    /// Writes `type-error.cast: {<square>} is not of type <circle> at
    /// typing:def…:s0`, or for an overflow, whose value is its operands,
    /// `type-error.overflow: <integer> arithmetic on #[…] overflows at …`.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.kind() == Some(TypeErrorKind::Overflow) {
            return write!(
                formatter,
                "{}: {} arithmetic on {} overflows at {}",
                self.code, self.expected, self.value, self.site
            );
        }
        write!(
            formatter,
            "{}: {} is not of type {} at {}",
            self.code, self.value, self.expected, self.site
        )
    }
}

impl InterlokoHost {
    /// The `<type-error>` class of this host's layout, if it has one.
    #[must_use]
    pub fn type_error_class(&self) -> Option<ClassId> {
        self.layout.class_id(ConditionClass::TypeError.name())
    }

    /// Makes the `<type-error>` a failed check at `site` signals for the
    /// rejected `values`: one value is the condition's value, and several
    /// (the operands of an operation whose result overflowed) a `<vector>`
    /// of them.
    pub fn make_type_error(
        &mut self,
        site: &TypeErrorSite,
        values: &[Value],
    ) -> Result<Obj, RuntimeError> {
        let class = self
            .type_error_class()
            .ok_or(RuntimeError::MissingBuiltinClass {
                name: ConditionClass::TypeError.name(),
            })?;
        let value = match values {
            [value] => self.box_top_value(value.clone())?,
            values => {
                let elements = values
                    .iter()
                    .map(|value| self.box_top_value(value.clone()))
                    .collect::<Result<Vec<_>, _>>()?;
                let vector = self
                    .push_vector(elements)
                    .map_err(|_| RuntimeError::CannotBoxValue)?;
                self.box_top_value(vector)?
            }
        };
        let mut text = |text: &str| {
            self.push_string(text.to_string())
                .map_err(|_| RuntimeError::MissingBuiltinClass { name: "<string>" })
        };
        let mut fields = [Value::Unit, Value::Unit, Value::Unit, Value::Unit];
        fields[TypeErrorSlot::Value.index()] = value;
        fields[TypeErrorSlot::ExpectedType.index()] = text(&site.expected)?;
        fields[TypeErrorSlot::Site.index()] = text(&site.site)?;
        fields[TypeErrorSlot::Code.index()] = text(site.kind.code())?;
        let refs = fields.iter().map(ValueRef::from_value).collect::<Vec<_>>();
        match self.make_object(class, &refs)? {
            Value::Obj(obj) => Ok(obj),
            _ => Err(RuntimeError::InvalidClass { class }),
        }
    }

    /// Reads what `condition` reports, when it is a `<type-error>` (or an
    /// instance of a subclass, whose layout starts with its slots).
    #[must_use]
    pub fn type_error_report(&self, condition: Obj) -> Option<TypeErrorReport> {
        let class = self.type_error_class()?;
        let object = self.object(condition).ok()?;
        if !self.layout.is_same_or_subclass(object.class, class) {
            return None;
        }
        let slot = |slot: TypeErrorSlot| {
            let id = SlotId(u32::try_from(slot.index()).ok()?);
            self.read_slot(condition, id).ok()
        };
        let text = |which: TypeErrorSlot| -> Option<String> {
            Some(self.string_text(&slot(which)?).ok()?.to_string())
        };
        Some(TypeErrorReport {
            code: text(TypeErrorSlot::Code)?,
            expected: text(TypeErrorSlot::ExpectedType)?,
            site: text(TypeErrorSlot::Site)?,
            value: self.describe_value(ValueRef::from_value(&slot(TypeErrorSlot::Value)?)),
        })
    }

    /// The runtime class of `value`, boxed or not: what `object-class`
    /// answers, so a chooser can pick a replacement of the right class.
    #[must_use]
    pub fn value_class(&self, value: &Value) -> Option<ClassId> {
        self.class_of_any(ValueRef::from_value(value)).ok()
    }

    /// Writes `value` for a report: Loko literal text for numbers,
    /// booleans, strings, symbols and vectors, a class's own name for a
    /// class object, and `{<class>}` for any other instance.
    #[must_use]
    pub fn describe_value(&self, value: ValueRef<'_>) -> String {
        match value {
            ValueRef::I64(value) => format!("{value}"),
            ValueRef::I32(value) => format!("{value}i32"),
            ValueRef::U32(value) => format!("{value}u32"),
            ValueRef::U64(value) => format!("{value}"),
            ValueRef::F64(value) => format!("{}", FloatBits::from_f64(value)),
            ValueRef::F32(value) => format!("{}", SingleFloatBits::from_f32(value)),
            ValueRef::Bool(true) => "#t".into(),
            ValueRef::Bool(false) => "#f".into(),
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_VALUE_TYPE => {
                match self.boxed_value(obj) {
                    Ok(inner) => self.describe_value(ValueRef::from_value(&inner)),
                    Err(_) => "{invalid}".into(),
                }
            }
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_OBJECT_TYPE => {
                self.describe_object(obj)
            }
            ValueRef::Func(_) | ValueRef::Closure(_) => "{<callable>}".into(),
            _ => "{value}".into(),
        }
    }

    fn describe_object(&self, obj: Obj) -> String {
        let Ok(object) = self.object(obj) else {
            return "{invalid}".into();
        };
        let class_name = |class: ClassId| {
            self.layout
                .class(class)
                .map_or_else(|| format!("class#{}", class.0), |class| class.name.clone())
        };
        match &object.storage {
            RuntimeObjectStorage::String { value } => string_literal(value),
            RuntimeObjectStorage::Symbol { name } => format!("#{name}"),
            RuntimeObjectStorage::Vector { elements } => {
                let elements = elements
                    .iter()
                    .map(|element| self.describe_value(ValueRef::from_value(element)))
                    .collect::<Vec<_>>();
                format!("#[{}]", elements.join(", "))
            }
            RuntimeObjectStorage::ClassMeta { class } => class_name(*class),
            _ => format!("{{{}}}", class_name(object.class)),
        }
    }
}

/// The error a checking host call returns when its check rejects the value:
/// [`CHECK_FAILED_TAG`] raised at the call when the calling program imports
/// it, so the program can signal a `<type-error>`, and a plain failure
/// otherwise.
pub(crate) fn check_failed(ctx: &HostContext<'_, '_>) -> HostError {
    let program = ctx.program();
    program
        .tag_imports
        .iter()
        .copied()
        .find(|tag| program.tag_name(*tag) == Some(CHECK_FAILED_TAG))
        .and_then(|tag| ctx.tag(tag))
        .map_or(HostError::Failed, |tag| HostError::throw(tag, Vec::new()))
}

/// Writes `text` as a Loko string literal.
fn string_literal(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for character in text.chars() {
        match character {
            '"' => literal.push_str("\\\""),
            '\\' => literal.push_str("\\\\"),
            '\n' => literal.push_str("\\n"),
            '\t' => literal.push_str("\\t"),
            character => literal.push(character),
        }
    }
    literal.push('"');
    literal
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use interloko_model::condition::TYPE_ERROR_SLOTS;
    use interloko_model::{OBJECT_CLASS_ID, SlotId};

    use super::*;
    use crate::{HostObjectKind, RuntimeClass, RuntimeLayout, RuntimeSlot, ValueKind};

    fn class(name: &str, superclass: Option<ClassId>, slots: Vec<RuntimeSlot>) -> RuntimeClass {
        RuntimeClass {
            name: name.into(),
            kind: HostObjectKind::Native,
            superclass,
            slots,
        }
    }

    /// `<object>`, `<string>`, `<vector>`, `<square>`, then the kernel
    /// condition classes.
    fn host() -> InterlokoHost {
        let type_error = ClassId(6);
        let slots = TYPE_ERROR_SLOTS
            .iter()
            .enumerate()
            .map(|(index, slot)| RuntimeSlot {
                name: slot.name.into(),
                type_name: slot.type_name.into(),
                kind: if index == TypeErrorSlot::Value.index() {
                    ValueKind::Any
                } else {
                    ValueKind::String(ClassId(1))
                },
                source_class: type_error,
                source_slot: SlotId(u32::try_from(index).expect("slot index")),
                init_keyword: slot.init_keyword.into(),
                mutable: false,
                getter: slot.name.into(),
                setter: None,
                host_backed: false,
            })
            .collect();
        InterlokoHost::new_checked(RuntimeLayout {
            classes: vec![
                class("<object>", None, Vec::new()),
                class("<string>", Some(OBJECT_CLASS_ID), Vec::new()),
                class("<vector>", Some(OBJECT_CLASS_ID), Vec::new()),
                class("<square>", Some(OBJECT_CLASS_ID), Vec::new()),
                class("<condition>", Some(OBJECT_CLASS_ID), Vec::new()),
                class("<error>", Some(ClassId(4)), Vec::new()),
                class("<type-error>", Some(ClassId(5)), slots),
            ],
            generics: Vec::new(),
            methods: Vec::new(),
        })
        .expect("layout")
    }

    fn site(kind: TypeErrorKind, expected: &str) -> TypeErrorSite {
        TypeErrorSite {
            kind,
            expected: expected.into(),
            site: "typing:def0000000000000001:s0".into(),
            span: 4..9,
        }
    }

    #[test]
    fn a_type_error_reports_its_value_type_site_and_code() {
        let mut host = host();
        let square = host.make_object(ClassId(3), &[]).expect("square");
        let condition = host
            .make_type_error(&site(TypeErrorKind::Cast, "<circle>"), &[square])
            .expect("type error");
        let report = host.type_error_report(condition).expect("report");
        assert_eq!(
            report,
            TypeErrorReport {
                code: "type-error.cast".into(),
                expected: "<circle>".into(),
                site: "typing:def0000000000000001:s0".into(),
                value: "{<square>}".into(),
            }
        );
        assert_eq!(report.kind(), Some(TypeErrorKind::Cast));
        assert_eq!(
            report.to_string(),
            "type-error.cast: {<square>} is not of type <circle> at typing:def0000000000000001:s0"
        );
        // Any other object reports nothing.
        let Value::Obj(square) = host.make_object(ClassId(3), &[]).expect("square") else {
            panic!("square is an object");
        };
        assert_eq!(host.type_error_report(square), None);
    }

    #[test]
    fn an_overflow_reports_its_operands() {
        let mut host = host();
        let condition = host
            .make_type_error(
                &site(TypeErrorKind::Overflow, "<integer>"),
                &[Value::I64(i64::MAX), Value::I64(1)],
            )
            .expect("type error");
        let report = host.type_error_report(condition).expect("report");
        assert_eq!(report.value, "#[9223372036854775807, 1]");
    }

    #[test]
    fn values_are_described_as_literals() {
        let mut host = host();
        let text = host.make_string("say \"hi\"\n").expect("string");
        let boxed = host.box_top_value(Value::F64(1.5)).expect("box");
        let describe = |value: &Value| host.describe_value(ValueRef::from_value(value));
        assert_eq!(describe(&text), r#""say \"hi\"\n""#);
        assert_eq!(describe(&boxed), "1.5");
        assert_eq!(describe(&Value::I32(-7)), "-7i32");
        assert_eq!(describe(&Value::Bool(false)), "#f");
    }

    #[test]
    fn a_layout_without_type_error_cannot_make_one() {
        let mut host = InterlokoHost::new_checked(RuntimeLayout {
            classes: vec![class("<object>", None, Vec::new())],
            generics: Vec::new(),
            methods: Vec::new(),
        })
        .expect("layout");
        assert_eq!(
            host.make_type_error(&site(TypeErrorKind::Range, "<i32>"), &[Value::I64(1)]),
            Err(RuntimeError::MissingBuiltinClass {
                name: "<type-error>"
            })
        );
    }
}
