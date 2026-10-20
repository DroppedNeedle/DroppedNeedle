//! Named SQL statements, declared so a test can check them against the
//! migrated schema.
//!
//! An area keeps its SQL in a file named `sql.rs` beside the code that runs
//! it. That file holds one [`sql!`](crate::sql) block and the functions that
//! build statement families, nothing else:
//!
//! ```ignore
//! droppedneedle_kernel::sql! {
//!     /// Fixed text other statements are built from.
//!     fragment RUN_FROM = " FROM library_scan_runs r";
//!     /// One scan run.
//!     pub(crate) RUN_BY_ID = "SELECT " + RUN_COLUMNS + RUN_FROM + " WHERE r.id = ?1";
//!     /// The run list, one statement per sort and direction.
//!     family RUN_PAGES = run_page;
//! }
//! ```
//!
//! A block whose statements run against a v2 database (the exporter's)
//! starts with `schema = V2;`, and the provider cache file's block with
//! `schema = Cache;`; every other block is written against the application
//! schema.
//!
//! A statement becomes a [`Statement`] constant and a fragment a `&str`
//! constant. `+` joins literals and `&str` constants at compile time, so a
//! statement built from a column list is still one `&'static str`. A family
//! names a function whose parameters are all [`Choice`] types and which
//! returns the statement for those choices; no caller can splice a runtime
//! string into it.
//!
//! The block also writes `registered()`, the file's list, compiled only for
//! tests and the `test-support` feature. Each crate that holds a `sql.rs`
//! joins its files' lists in one function at its root, and the schema test
//! prepares every statement, and every family over every combination of
//! its choices, against a copy of the migrated template, or for a
//! `schema = V2` block against the v2 fixture database (a `schema = Cache`
//! block against a file built from the cache schema). Preparing proves
//! the tables and columns a statement names exist and that the text is one
//! valid statement; it does not prove the call site binds the right number
//! of values, that a column list matches its row mapper, or the query plan.

/// One named statement. Pass [`Statement::text`] to rusqlite or sqlx.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Statement {
    name: &'static str,
    text: &'static str,
}

impl Statement {
    #[doc(hidden)]
    pub const fn new(name: &'static str, text: &'static str) -> Self {
        Self { name, text }
    }

    /// The module path and constant name, for the log and the schema test.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The SQL.
    pub const fn text(&self) -> &'static str {
        self.text
    }
}

/// A closed set of values a family's builder takes: every value, listed
/// once, so a test can build every statement the family can make.
pub trait Choice: Copy {
    /// Every value.
    fn all() -> Vec<Self>;
}

impl Choice for bool {
    fn all() -> Vec<Self> {
        vec![false, true]
    }
}

/// The length of an `IN (?, ?, ...)` list. The text differs only in the
/// number of placeholders, so the test builds lengths 1 and 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListLen(usize);

impl ListLen {
    /// A list of `len` values.
    pub const fn new(len: usize) -> Self {
        Self(len)
    }

    /// The number of placeholders.
    pub const fn get(self) -> usize {
        self.0
    }
}

impl Choice for ListLen {
    fn all() -> Vec<Self> {
        vec![Self(1), Self(3)]
    }
}

impl<A: Choice> Choice for (A,) {
    fn all() -> Vec<Self> {
        A::all().into_iter().map(|a| (a,)).collect()
    }
}

impl<A: Choice, B: Choice> Choice for (A, B) {
    fn all() -> Vec<Self> {
        let mut all = Vec::new();
        for a in A::all() {
            for b in B::all() {
                all.push((a, b));
            }
        }
        all
    }
}

impl<A: Choice, B: Choice, C: Choice> Choice for (A, B, C) {
    fn all() -> Vec<Self> {
        <(A, (B, C))>::all()
            .into_iter()
            .map(|(a, (b, c))| (a, b, c))
            .collect()
    }
}

impl<A: Choice, B: Choice, C: Choice, D: Choice> Choice for (A, B, C, D) {
    fn all() -> Vec<Self> {
        <(A, (B, C, D))>::all()
            .into_iter()
            .map(|(a, (b, c, d))| (a, b, c, d))
            .collect()
    }
}

/// The most statements one family may make. A family over more is split.
pub const MAX_COMBINATIONS: usize = 4096;

/// A family builder: a function of one to four [`Choice`] parameters that
/// returns a statement.
pub trait Builder<Args> {
    /// Every statement the builder makes, or the number of combinations
    /// when that is over [`MAX_COMBINATIONS`].
    fn every_text(&self) -> Result<Vec<String>, usize>;
}

macro_rules! builder_for {
    ($($arg:ident $value:ident),+) => {
        impl<F, $($arg: Choice),+> Builder<($($arg,)+)> for F
        where
            F: Fn($($arg),+) -> String,
        {
            fn every_text(&self) -> Result<Vec<String>, usize> {
                let all = <($($arg,)+) as Choice>::all();
                if all.len() > MAX_COMBINATIONS {
                    return Err(all.len());
                }
                Ok(all.into_iter().map(|($($value,)+)| self($($value),+)).collect())
            }
        }
    };
}

builder_for!(A a);
builder_for!(A a, B b);
builder_for!(A a, B b, C c);
builder_for!(A a, B b, C c, D d);

/// A family as the schema test sees it: its name and every statement it
/// makes.
#[derive(Debug, Clone, Copy)]
pub struct Family {
    name: &'static str,
    texts: fn() -> Result<Vec<String>, usize>,
}

impl Family {
    #[doc(hidden)]
    pub const fn new(name: &'static str, texts: fn() -> Result<Vec<String>, usize>) -> Self {
        Self { name, texts }
    }

    /// The module path and family name.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Every statement the family makes, or the number of combinations
    /// when that is over [`MAX_COMBINATIONS`].
    pub fn texts(&self) -> Result<Vec<String>, usize> {
        (self.texts)()
    }
}

/// The schema a `sql!` block's statements are written against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schema {
    /// The migrated application database.
    App,
    /// A v2 database, read by the exporter.
    V2,
    /// The provider cache file.
    Cache,
}

/// One declaration in a `sql!` block.
#[derive(Debug, Clone, Copy)]
pub enum Declared {
    /// A statement, prepared by the test.
    Statement(Statement),
    /// A fragment's name; a fragment is only ever part of other statements.
    Fragment(&'static str),
    /// A family, prepared over every combination.
    Family(Family),
}

/// The declarations of one `sql.rs`, in order.
#[derive(Debug, Clone)]
pub struct StatementFile {
    /// The file's module path, `droppedneedle_catalog::concerts::sql`.
    pub module: &'static str,
    /// The schema its statements are prepared against.
    pub schema: Schema,
    /// What the file declares.
    pub declared: Vec<Declared>,
}

/// An identifier from the database (a table name read from
/// `sqlite_schema`) quoted for SQL text. A name that cannot be listed ahead
/// of time goes through here and nowhere else.
pub fn quoted_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[doc(hidden)]
pub use const_format::concatcp as __concatcp;

/// Declare the statements, fragments and families of one `sql.rs`. See
/// [`db::statement`](crate::db::statement).
#[macro_export]
macro_rules! sql {
    (schema = $schema:ident; $($body:tt)*) => {
        $crate::__sql_items! { $schema [] $($body)* }
    };
    ($($body:tt)*) => {
        $crate::__sql_items! { App [] $($body)* }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __sql_items {
    (
        $schema:ident [$($done:tt)*]
        $(#[$meta:meta])* $vis:vis fragment $name:ident = $first:tt $(+ $part:tt)* ;
        $($rest:tt)*
    ) => {
        $(#[$meta])*
        $vis const $name: &str = $crate::db::statement::__concatcp!($first $(, $part)*);
        $crate::__sql_items! { $schema [$($done)* (fragment $name)] $($rest)* }
    };
    (
        $schema:ident [$($done:tt)*]
        $(#[$meta:meta])* family $name:ident = $builder:ident ;
        $($rest:tt)*
    ) => {
        $crate::__sql_items! { $schema [$($done)* (family $name $builder)] $($rest)* }
    };
    (
        $schema:ident [$($done:tt)*]
        $(#[$meta:meta])* $vis:vis $name:ident = $first:tt $(+ $part:tt)* ;
        $($rest:tt)*
    ) => {
        $(#[$meta])*
        $vis const $name: $crate::db::statement::Statement = $crate::db::statement::Statement::new(
            concat!(module_path!(), "::", stringify!($name)),
            $crate::db::statement::__concatcp!($first $(, $part)*),
        );
        $crate::__sql_items! { $schema [$($done)* (statement $name)] $($rest)* }
    };
    ($schema:ident [$($done:tt)*]) => {
        /// This file's declarations, for the schema test.
        #[cfg(any(test, feature = "test-support"))]
        pub(crate) fn registered() -> $crate::db::statement::StatementFile {
            $crate::db::statement::StatementFile {
                module: module_path!(),
                schema: $crate::db::statement::Schema::$schema,
                declared: vec![$($crate::__sql_declared!($done)),*],
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __sql_declared {
    ((fragment $name:ident)) => {
        $crate::db::statement::Declared::Fragment(concat!(module_path!(), "::", stringify!($name)))
    };
    ((statement $name:ident)) => {
        $crate::db::statement::Declared::Statement($name)
    };
    ((family $name:ident $builder:ident)) => {
        $crate::db::statement::Declared::Family($crate::sql_family!($name, $builder))
    };
}

/// A family entry for the schema test: the builder run over every
/// combination of its choices. Written by `sql!` for each `family` line.
#[macro_export]
macro_rules! sql_family {
    ($name:ident, $builder:ident) => {
        $crate::db::statement::Family::new(concat!(module_path!(), "::", stringify!($name)), || {
            $crate::db::statement::Builder::every_text(&$builder)
        })
    };
}

/// An enum or a struct whose values a family's builder takes. Writes the
/// type and its [`Choice`] list together, so there is no second list to
/// forget: every variant of an enum, every combination of a struct's
/// fields.
#[macro_export]
macro_rules! sql_choice {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $vis enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $crate::db::statement::Choice for $name {
            fn all() -> Vec<Self> {
                vec![$(Self::$variant),+]
            }
        }
    };
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $($(#[$field_meta:meta])* $field_vis:vis $field:ident : $ty:ty),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $vis struct $name {
            $($(#[$field_meta])* $field_vis $field: $ty),+
        }

        impl $crate::db::statement::Choice for $name {
            fn all() -> Vec<Self> {
                let mut all = Vec::new();
                $crate::__sql_choice_fields!(all; []; $($field: $ty),+);
                all
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __sql_choice_fields {
    ($all:ident; [$($done:ident)*]; $field:ident : $ty:ty $(, $rest:ident : $rest_ty:ty)*) => {
        for $field in <$ty as $crate::db::statement::Choice>::all() {
            $crate::__sql_choice_fields!($all; [$($done)* $field]; $($rest: $rest_ty),*);
        }
    };
    ($all:ident; [$($done:ident)*];) => {
        $all.push(Self { $($done),* });
    };
}
