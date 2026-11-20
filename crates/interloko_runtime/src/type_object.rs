//! Type objects in the runtime heap.
//!
//! A [`TypeObject`] is a class object, a `<limited-integer>`, a
//! `<subclass-type>`, a `<singleton>` or a `<union>`: a `<type-error>` holds
//! the type its failed check expected as one, and `instance?` tests values
//! against one. This module makes them, reads them back, and tests values
//! against them.

use alloc::string::{String, ToString};

use execution_tape::host::{HostError, ValueRef};
use execution_tape::value::Value;
use interloko_model::BuiltinValueClass;
use interloko_model::type_object::{SingletonValue, TypeObject};

use crate::{InterlokoHost, RuntimeError, RuntimeObject, RuntimeObjectStorage};

impl InterlokoHost {
    /// Makes the type object `ty`: a class object for a class, otherwise a
    /// `<limited-integer>`, `<subclass-type>`, `<singleton>` or `<union>`
    /// instance.
    pub fn make_type_object(&mut self, ty: &TypeObject) -> Result<Value, RuntimeError> {
        let missing =
            |kind: BuiltinValueClass| RuntimeError::MissingBuiltinClass { name: kind.name() };
        let (kind, storage) = match *ty {
            TypeObject::Class(class) => {
                return self.push_class_meta(class).map_err(|_| {
                    RuntimeError::MissingBuiltinClass {
                        name: BuiltinValueClass::Class.name(),
                    }
                });
            }
            TypeObject::LimitedInteger { range, named } => (
                BuiltinValueClass::LimitedIntegerType,
                RuntimeObjectStorage::LimitedIntegerType { range, named },
            ),
            TypeObject::Subclass(bound) => (
                BuiltinValueClass::SubclassType,
                RuntimeObjectStorage::SubclassType { bound },
            ),
            TypeObject::Singleton(_) => (
                BuiltinValueClass::SingletonType,
                RuntimeObjectStorage::CompoundType(ty.clone()),
            ),
            TypeObject::Union(_) => (
                BuiltinValueClass::UnionType,
                RuntimeObjectStorage::CompoundType(ty.clone()),
            ),
        };
        let class = self.builtin_class(kind).map_err(|_| missing(kind))?;
        Ok(self.push_object(RuntimeObject { class, storage }))
    }

    /// Reads the type object `value` is, boxed or not; `None` when it is not
    /// one.
    #[must_use]
    pub fn type_object(&self, value: ValueRef<'_>) -> Option<TypeObject> {
        self.type_object_from_any(value).ok()
    }

    pub(crate) fn type_object_from_any(
        &self,
        value: ValueRef<'_>,
    ) -> Result<TypeObject, HostError> {
        self.object_from_any(value)?
            .storage
            .type_object()
            .ok_or(HostError::Failed)
    }

    /// Whether `value`, boxed or not, is an instance of `ty`: of the class
    /// or a subclass, an `<integer>` in the range, a class object naming
    /// the bound or a subclass of it, the singleton's value, or an instance
    /// of a union's member.
    #[must_use]
    pub fn value_is_type_instance(&self, value: ValueRef<'_>, ty: &TypeObject) -> bool {
        match *ty {
            TypeObject::Class(class) => self.value_is_instance(value, class),
            TypeObject::LimitedInteger { range, .. } => self
                .integer_value(value)
                .is_some_and(|value| range.contains(value)),
            TypeObject::Subclass(bound) => self.object_from_any(value).is_ok_and(|object| {
                matches!(
                    object.storage,
                    RuntimeObjectStorage::ClassMeta { class }
                        if self.layout.is_same_or_subclass(class, bound)
                )
            }),
            TypeObject::Singleton(SingletonValue::Integer(expected)) => {
                self.integer_value(value) == Some(expected)
            }
            TypeObject::Singleton(SingletonValue::Boolean(expected)) => {
                self.boolean_value(value) == Some(expected)
            }
            TypeObject::Union(ref members) => members
                .iter()
                .any(|member| self.value_is_type_instance(value, member)),
        }
    }

    /// The boolean `value` holds, boxed or not.
    fn boolean_value(&self, value: ValueRef<'_>) -> Option<bool> {
        match value {
            ValueRef::Bool(value) => Some(value),
            ValueRef::Obj(obj) if obj.host_type == crate::INTERLOKO_VALUE_TYPE => {
                match self.boxed_value(obj).ok()? {
                    Value::Bool(value) => Some(value),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Writes `ty` as Loko writes it, with this host's class names:
    /// `<circle>`, `<i32>`, `limited(<integer>, min: 0, max: 255)` or
    /// `subclass(<shape>)`.
    #[must_use]
    pub fn type_object_name(&self, ty: &TypeObject) -> String {
        ty.display(&self.layout).to_string()
    }

    /// The integer `value` holds, boxed or not.
    fn integer_value(&self, value: ValueRef<'_>) -> Option<i64> {
        match value {
            ValueRef::I64(value) => Some(value),
            ValueRef::I32(value) => Some(i64::from(value)),
            ValueRef::U32(value) => Some(i64::from(value)),
            ValueRef::Obj(obj) if obj.host_type == crate::INTERLOKO_VALUE_TYPE => {
                match self.boxed_value(obj).ok()? {
                    Value::I64(value) => Some(value),
                    Value::I32(value) => Some(i64::from(value)),
                    Value::U32(value) => Some(i64::from(value)),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

impl RuntimeObjectStorage {
    /// The type object this storage is, when it is one.
    pub(crate) fn type_object(&self) -> Option<TypeObject> {
        match *self {
            Self::ClassMeta { class } => Some(TypeObject::Class(class)),
            Self::LimitedIntegerType { range, named } => {
                Some(TypeObject::LimitedInteger { range, named })
            }
            Self::SubclassType { bound } => Some(TypeObject::Subclass(bound)),
            Self::CompoundType(ref ty) => Some(ty.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use interloko_model::types::IntRange;
    use interloko_model::{ClassId, LimitedInteger, OBJECT_CLASS_ID};

    use super::*;
    use crate::{HostObjectKind, RuntimeClass, RuntimeLayout};

    fn class(name: &str, kind: HostObjectKind, superclass: Option<ClassId>) -> RuntimeClass {
        RuntimeClass {
            name: name.into(),
            kind,
            superclass,
            sealing: interloko_model::sealing::Sealing::Open,
            is_abstract: false,
            slots: Vec::new(),
        }
    }

    /// `<object>`, `<shape>`, `<circle>`, `<integer>`, the type classes,
    /// then `<boolean>`.
    fn host() -> InterlokoHost {
        let builtin = |kind: BuiltinValueClass, superclass| {
            class(
                kind.name(),
                HostObjectKind::BuiltinValue { kind },
                superclass,
            )
        };
        InterlokoHost::new_checked(RuntimeLayout {
            classes: vec![
                class("<object>", HostObjectKind::Native, None),
                class("<shape>", HostObjectKind::Native, Some(OBJECT_CLASS_ID)),
                class("<circle>", HostObjectKind::Native, Some(ClassId(1))),
                builtin(BuiltinValueClass::Integer, Some(OBJECT_CLASS_ID)),
                builtin(BuiltinValueClass::Type, Some(OBJECT_CLASS_ID)),
                builtin(BuiltinValueClass::Class, Some(ClassId(4))),
                builtin(BuiltinValueClass::LimitedIntegerType, Some(ClassId(4))),
                builtin(BuiltinValueClass::SubclassType, Some(ClassId(4))),
                builtin(BuiltinValueClass::SingletonType, Some(ClassId(4))),
                builtin(BuiltinValueClass::UnionType, Some(ClassId(4))),
                builtin(BuiltinValueClass::Boolean, Some(OBJECT_CLASS_ID)),
            ],
            generics: Vec::new(),
            methods: Vec::new(),
        })
        .expect("layout")
    }

    #[test]
    fn type_objects_round_trip_and_print() {
        let mut host = host();
        let byte = TypeObject::limited(IntRange {
            min: Some(0),
            max: Some(255),
        });
        for (ty, name) in [
            (TypeObject::Class(ClassId(2)), "<circle>"),
            (TypeObject::named(LimitedInteger::I32), "<i32>"),
            (byte, "limited(<integer>, min: 0, max: 255)"),
            (TypeObject::Subclass(ClassId(1)), "subclass(<shape>)"),
            (
                TypeObject::Singleton(SingletonValue::Boolean(false)),
                "singleton(#f)",
            ),
            (
                TypeObject::false_or(TypeObject::Class(ClassId(2))),
                "false-or(<circle>)",
            ),
        ] {
            let value = host.make_type_object(&ty).expect("type object");
            assert_eq!(
                host.type_object(ValueRef::from_value(&value)),
                Some(ty.clone())
            );
            assert_eq!(host.type_object_name(&ty), name);
            assert_eq!(host.describe_value(ValueRef::from_value(&value)), name);
        }
        assert_eq!(host.type_object(ValueRef::I64(1)), None);
    }

    #[test]
    fn values_are_tested_against_type_objects() {
        let mut host = host();
        let circle = host.make_object(ClassId(2), &[]).expect("circle");
        let shape_class = host
            .make_type_object(&TypeObject::Class(ClassId(1)))
            .expect("class object");
        let boxed = host.box_top_value(Value::I64(300)).expect("box");
        let byte = TypeObject::limited(IntRange {
            min: Some(0),
            max: Some(255),
        });
        let is = |host: &InterlokoHost, value: &Value, ty: TypeObject| {
            host.value_is_type_instance(ValueRef::from_value(value), &ty)
        };
        let byte = || byte.clone();
        assert!(is(&host, &circle, TypeObject::Class(ClassId(1))));
        assert!(!is(&host, &circle, TypeObject::Subclass(ClassId(1))));
        assert!(is(
            &host,
            &shape_class,
            TypeObject::Subclass(OBJECT_CLASS_ID)
        ));
        assert!(!is(&host, &shape_class, TypeObject::Subclass(ClassId(2))));
        assert!(is(&host, &Value::I64(255), byte()));
        assert!(is(&host, &Value::U32(7), byte()));
        assert!(!is(&host, &boxed, byte()));
        assert!(is(&host, &boxed, TypeObject::named(LimitedInteger::U32)));
        assert!(!is(&host, &circle, byte()));
        let maybe_circle = TypeObject::false_or(TypeObject::Class(ClassId(2)));
        let boxed_false = host.box_top_value(Value::Bool(false)).expect("box");
        assert!(is(&host, &circle, maybe_circle.clone()));
        assert!(is(&host, &Value::Bool(false), maybe_circle.clone()));
        assert!(is(&host, &boxed_false, maybe_circle.clone()));
        assert!(!is(&host, &Value::Bool(true), maybe_circle.clone()));
        assert!(!is(&host, &boxed, maybe_circle));
        let three = TypeObject::Singleton(SingletonValue::Integer(3));
        assert!(is(&host, &Value::I64(3), three.clone()));
        assert!(!is(&host, &Value::I64(4), three));
    }
}
