//! App passwords on the auth rig: seeded through the real store, read by
//! the compat password store the Subsonic and Jellyfin routes use.

use std::sync::Arc;

use droppedneedle_auth::compat_auth::prod::ProdCompatPasswords;
use droppedneedle_auth::session::tokens;
use droppedneedle_auth::users::models::AppPasswordRecord;
use droppedneedle_kernel::clock::Clock as _;

use super::auth::AuthRig;

impl AuthRig {
    /// Seed one live app password for `user_id` whose secret is `secret`.
    pub async fn seed_app_password(&self, user_id: &str, secret: &str) {
        let sha256 = tokens::hash_token(secret);
        let inserted = self
            .auth
            .app_passwords
            .insert_capped(
                AppPasswordRecord {
                    id: format!("app-{}", &sha256[..12]),
                    user_id: user_id.to_owned(),
                    name: "test".to_owned(),
                    secret_sha256: sha256,
                    secret_encrypted: self.deps.crypto.encrypt(secret).expect("secret seals"),
                    created_at: self.clock.now_unix(),
                    last_used_at: None,
                    last_client: None,
                },
                u64::MAX,
            )
            .await
            .expect("app password seeds");
        assert!(inserted, "app password under the cap");
    }

    /// The stored row for `secret`, while it is live.
    pub async fn app_password(&self, secret: &str) -> AppPasswordRecord {
        self.auth
            .app_passwords
            .get_active_by_sha256(&tokens::hash_token(secret))
            .await
            .expect("app password reads")
            .expect("app password is live")
    }

    /// The compat app-password store over the rig's users.
    pub fn compat_passwords(&self) -> ProdCompatPasswords {
        ProdCompatPasswords::new(self.deps.clone(), Arc::clone(&self.deps.crypto))
    }
}
