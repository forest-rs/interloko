//! Built-in functions as values.
//!
//! A call of a built-in function such as `digit?` or `sqrt` lowers to the
//! kernel's operation inline, picking the overload its arguments' types
//! select. Named as a value, a built-in is a function like any other, as
//! in Dylan:
//!
//! - one with a single overload, such as `digit?`, is a hidden kernel
//!   function whose body is that call of its parameters, made once per
//!   module, so every mention is the same value and `digit? == digit?`;
//! - one with several, such as `sqrt` (a `<double-float>` or a
//!   `<single-float>`) or `as-uppercase` (a `<character>` or a `<string>`),
//!   is a built-in generic with one kernel method per overload, each the
//!   call of its parameters. It is sealed over the classes those methods
//!   specialize on, as `size` is over the kernel's collections, so a
//!   program's method may join it for a class of its own. A call whose
//!   arguments typing cannot match to an overload, an `<object>` among
//!   them, sends to it.
//!
//! A few built-ins take arguments no callable signature can describe:
//! `apply` and `values` take any number, `make` the keywords of its class,
//! `as` and `limited` compute a type from one, and `signal` answers what
//! its handler supplies. Those are forms of the language, and naming one
//! as a value is an error. `copy-sequence` and `starts-with?`, whose
//! keywords default to positions of their string, are not values yet.

use alloc::vec;
use alloc::vec::Vec;

use super::{
    BinaryOp, FloatBuiltin, Intrinsic, Rounding, intrinsic_arity, intrinsic_expected_name,
    intrinsic_for_name, text,
};

/// The built-ins that are forms of the language rather than values; see
/// the module documentation.
pub const FORMS: &[&str] = &[
    "apply",
    "values",
    "make",
    "as",
    "limited",
    "signal",
    "copy-sequence",
    "starts-with?",
];

/// Whether `name` names a built-in a call can reach, a form or a function.
#[must_use]
pub fn names_built_in(name: &str) -> bool {
    FORMS.contains(&name) || overloads(name).is_some()
}

/// The overloads a value of the built-in function `name` covers, each as
/// its parameters' type names in order, or `None` when `name` names no
/// built-in function that is a value. A `%` name is the kernel's own and
/// never a value.
#[must_use]
pub fn overloads(name: &str) -> Option<Vec<Vec<&'static str>>> {
    const FLOATS: [&str; 2] = ["<double-float>", "<single-float>"];
    if name.starts_with('%') {
        return None;
    }
    if let Some(builtin) = FloatBuiltin::from_name(name) {
        // Every combination of float formats: mixed ones widen.
        let mut overloads = vec![Vec::new()];
        for _ in 0..builtin.arity() {
            overloads = overloads
                .into_iter()
                .flat_map(|prefix: Vec<&'static str>| {
                    FLOATS.iter().map(move |float| {
                        let mut params = prefix.clone();
                        params.push(*float);
                        params
                    })
                })
                .collect();
        }
        // Rounding an integer leaves it as it is.
        if matches!(
            builtin,
            FloatBuiltin::Floor
                | FloatBuiltin::Ceiling
                | FloatBuiltin::Round
                | FloatBuiltin::Truncate
        ) {
            overloads.push(vec!["<integer>"]);
        }
        return Some(overloads);
    }
    if let Some(functions) = text::text_functions_for_name(name) {
        return Some(
            functions
                .iter()
                .map(|function| {
                    function
                        .params()
                        .iter()
                        .map(|kind| text::text_kind_name(*kind))
                        .collect()
                })
                .collect(),
        );
    }
    if Rounding::from_name(name).is_some() {
        return Some(vec![vec!["<integer>", "<integer>"]]);
    }
    if let Some(op) = BinaryOp::bitwise_function(name) {
        // A shift is defined on one 32-bit type, and dispatch cannot tell
        // `<u32>` from `<i32>`, both `<integer>`s: the value shifts a
        // `<u32>`. The other bitwise functions are exact on any integer.
        return Some(vec![if op.is_shift() {
            vec!["<u32>", "<u32>"]
        } else {
            vec!["<integer>", "<integer>"]
        }]);
    }
    match name {
        "class-of" => return Some(vec![vec!["<object>"]]),
        "float-to-string" => return Some(FLOATS.iter().map(|float| vec![*float]).collect()),
        // `concatenate` takes a character for either string.
        "concatenate" => {
            let parts = ["<string>", "<character>"];
            return Some(
                parts
                    .iter()
                    .flat_map(|left| parts.iter().map(move |right| vec![*left, *right]))
                    .collect(),
            );
        }
        _ => {}
    }
    let intrinsic = intrinsic_for_name(name)?;
    if matches!(intrinsic, Intrinsic::Text(_)) {
        return None;
    }
    Some(vec![
        (0..intrinsic_arity(intrinsic))
            .map(|index| intrinsic_expected_name(intrinsic, index))
            .collect(),
    ])
}

#[cfg(test)]
mod tests {
    use super::{FORMS, overloads};
    use alloc::vec;

    #[test]
    fn single_and_overloaded_built_ins_are_told_apart() {
        assert_eq!(overloads("digit?"), Some(vec![vec!["<character>"]]));
        assert_eq!(
            overloads("as-uppercase"),
            Some(vec![vec!["<character>"], vec!["<string>"]])
        );
        assert_eq!(
            overloads("sqrt"),
            Some(vec![vec!["<double-float>"], vec!["<single-float>"]])
        );
        assert_eq!(overloads("atan2").map(|overloads| overloads.len()), Some(4));
        assert_eq!(overloads("floor").map(|overloads| overloads.len()), Some(3));
        assert_eq!(
            overloads("floor/"),
            Some(vec![vec!["<integer>", "<integer>"]])
        );
        assert_eq!(overloads("class-name"), Some(vec![vec!["<class>"]]));
        assert_eq!(overloads("%collection-size"), None);
        assert_eq!(overloads("no-such-function"), None);
        for form in FORMS {
            assert_eq!(overloads(form), None, "{form} is a form");
        }
    }
}
