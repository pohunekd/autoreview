//! Reads a persisted struct from a JSON object only; arrays and `null` are refused.

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
