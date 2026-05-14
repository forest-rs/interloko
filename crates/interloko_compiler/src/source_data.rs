//! Small structural data model for frontend protocol facts.
//!
//! Source data is inert. It is not executable syntax, not a macro system, and
//! not a semantic action result yet. The grammar frontend uses it to expose
//! parser/protocol facts in a shape future staged parsing can reuse.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// Inert structural data attached to source/frontend protocol records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceData {
    /// Symbolic identifier.
    Symbol(String),
    /// Text value.
    String(String),
    /// Signed integer value.
    Integer(i64),
    /// Boolean value.
    Bool(bool),
    /// Ordered data list.
    List(Vec<SourceData>),
    /// Tagged record with named fields.
    Record(SourceDataRecord),
}

impl SourceData {
    /// Creates a symbolic identifier.
    #[must_use]
    pub fn symbol(value: impl Into<String>) -> Self {
        Self::Symbol(value.into())
    }

    /// Creates a text value.
    #[must_use]
    pub fn string(value: impl Into<String>) -> Self {
        Self::String(value.into())
    }

    /// Creates a signed integer value.
    #[must_use]
    pub const fn integer(value: i64) -> Self {
        Self::Integer(value)
    }

    /// Creates a boolean value.
    #[must_use]
    pub const fn bool(value: bool) -> Self {
        Self::Bool(value)
    }

    /// Creates an ordered data list.
    #[must_use]
    pub fn list(items: Vec<SourceData>) -> Self {
        Self::List(items)
    }

    /// Creates a tagged record.
    #[must_use]
    pub fn record(tag: impl Into<String>, fields: Vec<SourceDataField>) -> Self {
        Self::Record(SourceDataRecord {
            tag: tag.into(),
            fields,
        })
    }
}

impl fmt::Display for SourceData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symbol(value) => formatter.write_str(value),
            Self::String(value) => write!(formatter, "{value:?}"),
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::Bool(true) => formatter.write_str("#t"),
            Self::Bool(false) => formatter.write_str("#f"),
            Self::List(items) => {
                formatter.write_str("[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{item}")?;
                }
                formatter.write_str("]")
            }
            Self::Record(record) => write!(formatter, "{record}"),
        }
    }
}

/// Tagged record value in [`SourceData`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceDataRecord {
    /// Record tag.
    pub tag: String,
    /// Ordered record fields.
    pub fields: Vec<SourceDataField>,
}

impl fmt::Display for SourceDataRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}(", self.tag)?;
        for (index, field) in self.fields.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{field}")?;
        }
        formatter.write_str(")")
    }
}

/// Named field in a [`SourceDataRecord`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceDataField {
    /// Field name.
    pub name: String,
    /// Field value.
    pub value: SourceData,
}

impl SourceDataField {
    /// Creates a named source-data field.
    #[must_use]
    pub fn new(name: impl Into<String>, value: SourceData) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }
}

impl fmt::Display for SourceDataField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.name, self.value)
    }
}

/// Creates a named source-data field.
#[must_use]
pub fn field(name: impl Into<String>, value: SourceData) -> SourceDataField {
    SourceDataField::new(name, value)
}

/// Renders source data into a stable text form for dumps and fingerprints.
#[must_use]
pub fn dump(data: &SourceData) -> String {
    format!("{data}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn dumps_nested_source_data() {
        let data = SourceData::record(
            "grammar-rule",
            vec![
                field("name", SourceData::symbol("function-definition")),
                field("direct", SourceData::bool(true)),
                field(
                    "parts",
                    SourceData::list(vec![
                        SourceData::string("define function"),
                        SourceData::integer(1),
                    ]),
                ),
            ],
        );

        assert_eq!(
            dump(&data),
            "grammar-rule(name: function-definition, direct: #t, parts: [\"define function\", 1])"
        );
    }
}
