//! The one SQL conversion for a closed set stored as text.

/// Give a fieldless enum its stored spelling: `ALL`, every value in the
/// order listed; `as_str` to write one; a `parse` that answers `None` for
/// any other text; and the codec that binds and reads it, where any other
/// text is a decode error. `as_str` matches every variant, so a variant
/// left out of the list does not compile, and `ALL` is complete.
///
/// A column holding one of these carries a CHECK listing exactly `ALL`'s
/// spellings; the schema test compares the two.
#[macro_export]
macro_rules! spelled {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            /// Every value, in the order listed.
            pub const ALL: [Self; [$($text),+].len()] = [$($name::$variant),+];

            /// The stored spelling.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            /// The value `text` spells, if any.
            pub fn parse(text: &str) -> Option<Self> {
                match text {
                    $($text => Some($name::$variant),)+
                    _ => None,
                }
            }
        }

        impl $crate::db::statement::__rusqlite::types::ToSql for $name {
            fn to_sql(
                &self,
            ) -> $crate::db::statement::__rusqlite::Result<
                $crate::db::statement::__rusqlite::types::ToSqlOutput<'_>,
            > {
                Ok($crate::db::statement::__rusqlite::types::ToSqlOutput::from(self.as_str()))
            }
        }

        impl $crate::db::statement::__rusqlite::types::FromSql for $name {
            fn column_result(
                value: $crate::db::statement::__rusqlite::types::ValueRef<'_>,
            ) -> $crate::db::statement::__rusqlite::types::FromSqlResult<Self> {
                let text = value.as_str()?;
                Self::parse(text).ok_or_else(|| {
                    $crate::db::statement::__rusqlite::types::FromSqlError::Other(
                        format!("{} has no value {text:?}", stringify!($name)).into(),
                    )
                })
            }
        }
    };
}
