//! SQLite schema: embedded migrations and boot checks.
//!
//! Every schema change ships as a versioned `server/migrations/*.sql` file,
//! embedded at compile time and applied at boot before serving traffic.
//! There are no down migrations; rollback is restoring a pre-upgrade backup.
//!
//! `PRAGMA user_version` is the migration high-water mark: each migration
//! ends by stamping its version, and [`boot::assert_migrated`] refuses to
//! serve when the database stamp differs from the binary.

pub mod boot;

#[cfg(any(test, feature = "test-support"))]
pub use boot::apply_migrations_blocking;
pub use boot::{SchemaError, apply_migrations, assert_migrated, latest_version};

/// Embedded migrations, applied in version order by [`apply_migrations`].
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
