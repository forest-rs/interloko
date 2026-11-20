//! Lists: `<pair>`s and the empty list, `#()`.
//!
//! A `<pair>` is a runtime object of two slots, its head and its tail, each
//! any object boxed as an `<object>`: native slot storage, as an instance
//! has, so turns log and undo stores into image pairs and evacuate the
//! values pairs hold exactly as they do an instance's. The empty list is a
//! kernel marker of the class `<empty-list>`: one object, made in the image
//! the first time anything asks for it, so `#() == #()` and testing for
//! the end of a list is a handle compare.
//!
//! As in Dylan, `head` and `tail` of the empty list are the empty list;
//! storing into it is an error the compiler rules out, since the setters
//! take a `<pair>`.

use alloc::vec;

use execution_tape::host::{HostError, ValueRef};
use execution_tape::value::{Obj, Value};

use crate::{
    BuiltinValueClass, INTERLOKO_OBJECT_TYPE, InterlokoHost, Marker, RuntimeObject,
    RuntimeObjectStorage, region,
};

/// Which half of a pair an operation reads or writes: the head is slot 0,
/// the tail slot 1.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PairPart {
    Head = 0,
    Tail = 1,
}

impl InterlokoHost {
    /// `pair(head, tail)`: a new `<pair>` holding the two boxed values.
    pub(crate) fn make_pair(&mut self, head: Value, tail: Value) -> Result<Value, HostError> {
        let class = self.builtin_class(BuiltinValueClass::Pair)?;
        Ok(self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::Native {
                slots: vec![head, tail],
            },
        }))
    }

    /// `#()`, the empty list.
    pub(crate) fn empty_list(&mut self) -> Obj {
        self.marker(Marker::EmptyList)
    }

    /// Whether `object` is a `<pair>`.
    pub(crate) fn is_pair_object(&self, object: Obj) -> bool {
        object.host_type == INTERLOKO_OBJECT_TYPE
            && self
                .builtin_class(BuiltinValueClass::Pair)
                .is_ok_and(|pair| self.object(object).is_ok_and(|object| object.class == pair))
    }

    /// `head(list)` or `tail(list)`: the part of a pair, or the empty list
    /// itself for the empty list, as a boxed `<object>`.
    pub(crate) fn list_part(&mut self, list: Obj, part: PairPart) -> Result<Value, HostError> {
        if self.is_marker(ValueRef::Obj(list), Marker::EmptyList) {
            return Ok(self.push_boxed_value(Value::Obj(list)));
        }
        if !self.is_pair_object(list) {
            return Err(HostError::Failed);
        }
        match &self.object(list).map_err(|_| HostError::Failed)?.storage {
            RuntimeObjectStorage::Native { slots } => {
                slots.get(part as usize).cloned().ok_or(HostError::Failed)
            }
            _ => Err(HostError::Failed),
        }
    }

    /// `head-setter(value, pair)` or `tail-setter(value, pair)`: stores the
    /// boxed `value` in the pair's part and returns it. A store into an
    /// image pair during a turn is logged, as a slot store is.
    pub(crate) fn set_pair_part(
        &mut self,
        value: Value,
        pair: Obj,
        part: PairPart,
    ) -> Result<Value, HostError> {
        if !self.is_pair_object(pair) {
            return Err(HostError::Failed);
        }
        let location = self.object_ref(pair).map_err(|_| HostError::Failed)?;
        let old = match &self.object(pair).map_err(|_| HostError::Failed)?.storage {
            RuntimeObjectStorage::Native { slots } => {
                slots.get(part as usize).cloned().ok_or(HostError::Failed)?
            }
            _ => return Err(HostError::Failed),
        };
        self.log_store(location.space, || region::LoggedStore::Slot {
            index: location.index,
            slot: part as usize,
            old,
        });
        let RuntimeObjectStorage::Native { slots } = &mut self
            .object_mut(pair)
            .map_err(|_| HostError::Failed)?
            .storage
        else {
            return Err(HostError::Failed);
        };
        slots[part as usize] = value.clone();
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use execution_tape::host::ValueRef;
    use execution_tape::value::{Obj, Value};
    use interloko_model::sealing::Sealing;
    use interloko_model::{BuiltinValueClass, ClassId, OBJECT_CLASS_ID};

    use super::PairPart;
    use crate::{HostObjectKind, InterlokoHost, RuntimeClass, RuntimeLayout};

    /// `<object>`, then `<list>`, `<pair>` and `<empty-list>`.
    fn host() -> InterlokoHost {
        let builtin = |kind: BuiltinValueClass, superclass| RuntimeClass {
            name: kind.name().into(),
            kind: HostObjectKind::BuiltinValue { kind },
            superclass: Some(superclass),
            sealing: Sealing::Sealed,
            is_abstract: kind == BuiltinValueClass::List,
            slots: Vec::new(),
        };
        let layout = RuntimeLayout {
            classes: vec![
                RuntimeClass {
                    name: "<object>".into(),
                    kind: HostObjectKind::Native,
                    superclass: None,
                    sealing: Sealing::Open,
                    is_abstract: false,
                    slots: Vec::new(),
                },
                builtin(BuiltinValueClass::List, OBJECT_CLASS_ID),
                builtin(BuiltinValueClass::Pair, ClassId(1)),
                builtin(BuiltinValueClass::EmptyList, ClassId(1)),
            ],
            generics: Vec::new(),
            methods: Vec::new(),
        };
        InterlokoHost::new_checked(layout).expect("layout")
    }

    fn object(value: &Value) -> Obj {
        let Value::Obj(object) = value else {
            panic!("expected an object, got {value:?}");
        };
        *object
    }

    fn int(host: &mut InterlokoHost, value: i64) -> Value {
        host.box_top_value(Value::I64(value)).expect("boxes")
    }

    #[test]
    fn the_empty_list_is_one_object_of_its_own_class() {
        let mut host = host();
        let empty = host.empty_list();
        assert_eq!(host.empty_list(), empty);
        assert_eq!(host.object(empty).expect("object").class, ClassId(3));
        // Its head and tail are itself, boxed.
        for part in [PairPart::Head, PairPart::Tail] {
            let value = host.list_part(empty, part).expect("reads");
            assert_eq!(host.value_object(ValueRef::from_value(&value)), Some(empty));
        }
        assert!(
            host.set_pair_part(Value::Bool(true), empty, PairPart::Head)
                .is_err()
        );
    }

    #[test]
    fn pairs_hold_their_parts_and_aborted_turns_restore_them() {
        let mut host = host();
        let one = int(&mut host, 1);
        let empty = host.empty_list();
        let tail = host.push_boxed_value(Value::Obj(empty));
        let pair = object(&host.make_pair(one.clone(), tail.clone()).expect("pair"));
        assert_eq!(host.object(pair).expect("object").class, ClassId(2));
        assert_eq!(host.list_part(pair, PairPart::Head).expect("head"), one);
        assert_eq!(host.list_part(pair, PairPart::Tail).expect("tail"), tail);

        host.begin_turn().expect("turn opens");
        let two = int(&mut host, 2);
        host.set_pair_part(two.clone(), pair, PairPart::Head)
            .expect("stores");
        host.set_pair_part(two.clone(), pair, PairPart::Tail)
            .expect("stores");
        assert_eq!(host.list_part(pair, PairPart::Head).expect("head"), two);
        let stats = host.abort_turn().expect("turn aborts");
        assert_eq!(stats.logged_stores, 2);
        assert_eq!(host.list_part(pair, PairPart::Head).expect("head"), one);
        assert_eq!(host.list_part(pair, PairPart::Tail).expect("tail"), tail);
    }
}
