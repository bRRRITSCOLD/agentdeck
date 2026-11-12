//! The JSON schemas behind the `schemars` feature's `JsonSchema` impls: each type's schema
//! matches its serde form.

use schemars::schema::{InstanceType, NumberValidation, Schema, SchemaObject};

/// An id: a UUID as its hyphenated string.
pub(crate) fn uuid_string() -> Schema {
    SchemaObject {
        instance_type: Some(InstanceType::String.into()),
        format: Some("uuid".to_owned()),
        ..Default::default()
    }
    .into()
}

/// A text enum: exactly one of `values`.
pub(crate) fn one_of_strings(values: &[&str]) -> Schema {
    SchemaObject {
        instance_type: Some(InstanceType::String.into()),
        enum_values: Some(values.iter().map(|value| (*value).into()).collect()),
        ..Default::default()
    }
    .into()
}

/// A split ratio: a number strictly between 0 and 1.
pub(crate) fn open_unit_interval() -> Schema {
    SchemaObject {
        instance_type: Some(InstanceType::Number.into()),
        format: Some("float".to_owned()),
        number: Some(Box::new(NumberValidation {
            exclusive_minimum: Some(0.0),
            exclusive_maximum: Some(1.0),
            ..Default::default()
        })),
        ..Default::default()
    }
    .into()
}

#[cfg(test)]
mod tests {
    use schemars::schema_for;
    use serde_json::{Value, json};

    use crate::{Layout, TaskStatus, WorkspaceId};

    fn schema<T: schemars::JsonSchema>() -> Value {
        serde_json::to_value(schema_for!(T)).unwrap()
    }

    mod json_schema {
        use super::*;

        #[test]
        fn should_describe_an_id_as_a_uuid_string() {
            let schema = schema::<WorkspaceId>();
            assert_eq!(schema["type"], "string");
            assert_eq!(schema["format"], "uuid");
        }

        #[test]
        fn should_describe_a_text_enum_by_its_column_words() {
            assert_eq!(
                schema::<TaskStatus>()["enum"],
                json!(["todo", "doing", "review", "done"])
            );
        }

        #[test]
        fn should_bound_a_split_ratio_strictly_between_zero_and_one() {
            let schema = schema::<Layout>();
            let ratio = &schema["anyOf"][1]["properties"]["ratio"];
            assert_eq!(ratio["exclusiveMinimum"], 0.0);
            assert_eq!(ratio["exclusiveMaximum"], 1.0);
        }
    }
}
