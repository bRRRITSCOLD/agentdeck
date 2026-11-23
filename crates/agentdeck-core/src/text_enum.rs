//! `text_enum!`: an enum stored as one lowercase word, the same in SQLite, JSON and TOML.

use thiserror::Error;

/// A string that names no variant of the enum it was parsed as.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown {kind} `{value}`")]
pub struct UnknownValue {
    /// The enum's name, e.g. `pane kind`.
    pub kind: &'static str,
    /// The string that was given.
    pub value: String,
}

/// Defines an enum whose variants each have one text form. `as_str`, `Display`, `FromStr` and
/// serde all use that form, so the database column, the JSON field and the TOML value cannot
/// drift apart.
macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident ($kind:literal) {
            $( $(#[$vmeta:meta])* $variant:ident => $text:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The variant's text form.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( $name::$variant => $text, )+
                }
            }
        }

        #[cfg(feature = "schemars")]
        impl ::schemars::JsonSchema for $name {
            fn schema_name() -> ::std::string::String {
                stringify!($name).to_owned()
            }

            fn json_schema(
                _: &mut ::schemars::r#gen::SchemaGenerator,
            ) -> ::schemars::schema::Schema {
                $crate::schema::one_of_strings(&[$($text),+])
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::text_enum::UnknownValue;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( $text => Ok($name::$variant), )+
                    other => Err($crate::text_enum::UnknownValue {
                        kind: $kind,
                        value: other.to_owned(),
                    }),
                }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <::std::string::String>::deserialize(deserializer)?;
                text.parse().map_err(::serde::de::Error::custom)
            }
        }
    };
}

pub(crate) use text_enum;
