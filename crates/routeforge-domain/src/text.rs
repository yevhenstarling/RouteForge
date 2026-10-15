//! Enumerations that are stored and sent as text.

/// A text that names no value of an enumeration.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("`{text}` is not a {expected}")]
pub struct UnknownVariant {
    /// The name of the enumeration.
    pub expected: &'static str,
    pub text: String,
}

/// Declares an enumeration together with the text of each value.
///
/// The same text is used in JSON, in the database, in the OpenAPI document and by
/// `Display` and `FromStr`, so they cannot drift apart. The schema repeats it in a
/// `CHECK`, and a test in `db` inserts every value to keep the two in step.
///
/// The text is taken as a token and not as a `literal`. A literal is handed on wrapped
/// in an invisible group, which serde looks into and utoipa does not: the document
/// would name the values `Queued` and `Routing`, as the variants are spelled.
macro_rules! text_enum {
    (
        $(#[$doc:meta])*
        $name:ident {
            $($(#[$variant_doc:meta])* $variant:ident => $text:tt),+ $(,)?
        }
    ) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize)]
        #[cfg_attr(feature = "openapi", derive(::utoipa::ToSchema))]
        pub enum $name {
            $(
                $(#[$variant_doc])*
                #[serde(rename = $text)]
                $variant,
            )+
        }

        impl $name {
            /// Every value, in the order declared.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The text this value is stored and sent as.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::text::UnknownVariant;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                match text {
                    $($text => Ok(Self::$variant),)+
                    _ => Err($crate::text::UnknownVariant {
                        expected: stringify!($name),
                        text: text.to_owned(),
                    }),
                }
            }
        }
    };
}

pub(crate) use text_enum;

#[cfg(test)]
pub(crate) mod testing {
    use std::fmt::{Debug, Display};
    use std::str::FromStr;

    use serde::Serialize;
    use serde::de::DeserializeOwned;

    use super::UnknownVariant;

    /// Checks that `Display`, `FromStr` and serde agree on the text of every value.
    pub(crate) fn assert_text_round_trips<T>(all: &[T])
    where
        T: Copy + Debug + PartialEq + Display + Serialize + DeserializeOwned,
        T: FromStr<Err = UnknownVariant>,
    {
        for &value in all {
            let text = value.to_string();
            assert_eq!(text.parse::<T>().unwrap(), value);
            assert_eq!(serde_json::to_value(value).unwrap(), text.as_str());
            assert_eq!(
                serde_json::from_value::<T>(text.as_str().into()).unwrap(),
                value
            );
            assert_eq!(text, text.to_lowercase(), "values are stored in lower case");
        }
        let unknown = "no such value".parse::<T>().unwrap_err();
        assert_eq!(unknown.text, "no such value");
    }
}
