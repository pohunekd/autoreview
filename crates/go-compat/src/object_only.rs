//! Reads a persisted struct from a JSON object only, as Go's `encoding/json` does.

// Rust guideline compliant 2026-06-26

/// Implements `Serialize` and `Deserialize` for a struct that uses `#[serde(remote = "Self")]`.
///
/// The derive emits inherent `serialize` and `deserialize` functions, and these trait impls call
/// them. `Deserialize` accepts only a JSON object, so a JSON array or `null` is refused with an
/// error that names the struct.
#[macro_export]
macro_rules! object_only_serde {
    ($name:ident) => {
        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                $name::serialize(self, serializer)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Self, D::Error> {
                struct ObjectVisitor;

                impl<'de> ::serde::de::Visitor<'de> for ObjectVisitor {
                    type Value = $name;

                    fn expecting(
                        &self,
                        formatter: &mut ::core::fmt::Formatter<'_>,
                    ) -> ::core::fmt::Result {
                        write!(formatter, "a JSON object for {}", stringify!($name))
                    }

                    fn visit_map<A: ::serde::de::MapAccess<'de>>(
                        self,
                        map: A,
                    ) -> Result<$name, A::Error> {
                        $name::deserialize(::serde::de::value::MapAccessDeserializer::new(map))
                    }
                }

                deserializer.deserialize_map(ObjectVisitor)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(default, remote = "Self")]
    struct Verdict {
        status: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        relies_on: String,
    }

    crate::object_only_serde!(Verdict);

    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(default, remote = "Self")]
    struct Wrapped {
        key: String,
        #[serde(flatten)]
        verdict: Verdict,
        #[serde(
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "crate::null_default::deserialize"
        )]
        items: Vec<Verdict>,
        scope: Verdict,
    }

    crate::object_only_serde!(Wrapped);

    #[test]
    fn flatten_fields_fill_and_nested_arrays_are_refused() {
        let filled: Wrapped =
            serde_json::from_str(r#"{"key":"k","status":"s","relies_on":"r"}"#).expect("flatten");
        assert_eq!(filled.verdict.status, "s");
        assert_eq!(filled.verdict.relies_on, "r");

        assert!(serde_json::from_str::<Wrapped>(r#"{"scope": []}"#).is_err());
        assert!(serde_json::from_str::<Wrapped>(r#"{"items": [[]]}"#).is_err());
        let nulled: Wrapped = serde_json::from_str(r#"{"items": null}"#).expect("null list");
        assert!(nulled.items.is_empty());
    }
}
