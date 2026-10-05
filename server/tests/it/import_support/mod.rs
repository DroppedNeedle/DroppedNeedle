//! Shared harness for the import tests: sealed export fixtures, scratch
//! databases, and scratch config dirs.
//!
//! Fixtures seal through the real exporter sealer
//! (`droppedneedle::export::seal::Sealer`), so every test also proves the
//! exporter and importer agree on the wire format. Databases are in-memory
//! pools; config files live in a `ScratchDir` that is removed on drop.

use crate::common::ScratchDir;
use std::path::PathBuf;

use droppedneedle::export::seal::Sealer;
use droppedneedle::runtime_config::Crypto;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

/// Operator passphrase sealing every fixture.
pub const PASSPHRASE: &str = "correct horse battery staple for tests";

/// A fixed bcrypt hash (cost 4, nonsense salt) for local-provider rows.
pub const BCRYPT_HASH: &str = "$2b$04$abcdefghijklmnopqrstuu012345678901234567890123456789";

/// A well-formed MBID for follow/approval rows.
pub const MBID: &str = "01234567-89ab-cdef-0123-456789abcdef";

/// One sealing context plus fixture builders bound to it.
pub struct Fixture {
    sealer: Sealer,
}

impl Fixture {
    /// Build a sealer. One Argon2id derivation; briefs share the
    /// fixture across their seals.
    pub fn new() -> Self {
        Self {
            sealer: Sealer::generate(PASSPHRASE).unwrap(),
        }
    }

    /// Seal one secret into a `{ "$sealed": ... }` object.
    pub fn seal(&self, plaintext: &str) -> Value {
        json!({ "$sealed": self.sealer.seal(plaintext).unwrap() })
    }

    /// The `secret_envelope` block matching this sealer.
    pub fn envelope(&self) -> Value {
        serde_json::to_value(self.sealer.secret_envelope()).unwrap()
    }

    /// A complete export document shell; callers fill the entities.
    pub fn shell(&self) -> Value {
        json!({
            "format": "droppedneedle-export",
            "format_version": 1,
            "exported_at": "2026-09-28T12:00:00Z",
            "v2_commit": "cf7278a1",
            "instance_id": "instance-1",
            "secret_envelope": self.envelope(),
            "users": [],
            "settings": {},
            "follows": [],
            "approvals": [],
        })
    }

    /// A minimal valid user row with one local provider, one app
    /// password, and one recovery code.
    pub fn user(&self, id: &str) -> Value {
        let provider_data = format!(r#"{{"password_hash": "{BCRYPT_HASH}"}}"#);
        json!({
            "id": id,
            "display_name": format!("User {id}"),
            "email": format!("{id}@example.com"),
            "avatar_url": null,
            "role": "user",
            "username": id,
            "username_display": id,
            "created_at": "2026-01-01T00:00:00+00:00",
            "last_login_at": null,
            "providers": [
                {
                    "provider": "local",
                    "provider_uid": format!("local:{id}"),
                    "provider_data": provider_data,
                    "hash_scheme": "bcrypt",
                    "created_at": "2026-01-01T00:00:00+00:00",
                }
            ],
            "app_passwords": [
                {
                    "name": "phone",
                    "secret": self.seal(&format!("app-secret-{id}")),
                    "created_at": "2026-02-01T00:00:00+00:00",
                    "last_used_at": null,
                    "last_client": null,
                    "revoked": false,
                }
            ],
            "recovery_code": {
                "code_hash": format!("hash-{id}"),
                "created_at": "2026-03-01T00:00:00+00:00",
                "expires_at": "2026-04-01T00:00:00+00:00",
            },
        })
    }

    /// A follow row for `user_id` and `mbid`.
    pub fn follow(&self, user_id: &str, mbid: &str) -> Value {
        json!({
            "user_id": user_id,
            "artist_mbid": mbid,
            "artist_name": "Test Artist",
            "auto_download": false,
            "followed_at": 1_700_000_000.0,
            "updated_at": 1_700_000_100.0,
        })
    }

    /// An approval row for `user_id` and `mbid`.
    pub fn approval(&self, user_id: &str, mbid: &str) -> Value {
        json!({
            "user_id": user_id,
            "artist_mbid": mbid,
            "artist_name": "Test Artist",
            "state": "pending",
            "requested_at": 1_700_000_000.0,
            "reviewed_by_id": null,
            "reviewed_by_name": null,
            "reviewed_at": null,
            "batch_id": null,
            "source": null,
        })
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

/// One-connection in-memory pool with migrations applied.
pub async fn migrated_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    droppedneedle::schema::apply_migrations(&pool)
        .await
        .unwrap();
    pool
}

/// Deterministic v3 key for briefs. `Crypto` is not cloneable by
/// design (it wipes on drop), so briefs mint one per import.
pub fn test_crypto() -> Crypto {
    Crypto::from_key_bytes(&[7u8; 32]).unwrap()
}

/// Scratch config dir with a deterministic v3 key. Returns the dir guard
/// (removed on drop, so keep it alive), the config file path and the
/// crypto handle.
pub fn scratch_config(tag: &str) -> (ScratchDir, PathBuf, Crypto) {
    let dir = ScratchDir::new(&format!("import-{tag}"));
    let config_path = dir.join("config.json");
    let crypto = Crypto::from_key_bytes(&[7u8; 32]).unwrap();
    (dir, config_path, crypto)
}

/// Standard import request over scratch state.
pub fn import_request(
    export: &Value,
    pool: SqlitePool,
    config_path: PathBuf,
    crypto: Crypto,
) -> droppedneedle::import::ImportRequest {
    droppedneedle::import::ImportRequest {
        export_bytes: serde_json::to_vec(export).unwrap(),
        passphrase: PASSPHRASE.to_owned(),
        pool,
        config_path,
        crypto,
        v2_config_path: None,
        dry_run: false,
        fault_before_commit: false,
        fault_after_commit: false,
    }
}

/// Row count helper for atomicity briefs.
pub async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}
