//! Briefs for the reads every auth repository shares.

use droppedneedle_kernel::db::{Db, DbError, Lane};

/// A person counts as seen by a session used after the time, or an app
/// password used after it, whichever is newer; one seen by neither, or
/// only before it, does not.
#[tokio::test]
async fn seen_since_reads_sessions_and_app_passwords() {
    let db = Db::scratch("auth-seen-since").unwrap();
    db.write(Lane::Foreground, "test.seed", |tx| {
        tx.execute_batch(
            "INSERT INTO auth_users (id, display_name, created_at) VALUES
                 ('by-session', 'A', 1), ('by-password', 'B', 1), ('old', 'C', 1),
                 ('never', 'D', 1);
             INSERT INTO auth_tokens (id, user_id, token_hash, issued_at, expires_at,
                 last_seen_at, revoked) VALUES
                 ('t1', 'by-session', 'h1', 1, 9000, 5000, 0),
                 ('t2', 'old', 'h2', 1, 9000, 900, 0),
                 ('t3', 'by-password', 'h3', 1, 9000, 800, 0);
             INSERT INTO auth_app_passwords (id, user_id, name, secret_sha256,
                 secret_encrypted, created_at, last_used_at, revoked) VALUES
                 ('p1', 'by-password', 'app', 's1', '', 1, 4000, 0),
                 ('p2', 'old', 'app', 's2', '', 1, NULL, 0);",
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();

    assert_eq!(
        super::seen_since(&db, 1000).await.unwrap(),
        ["by-password", "by-session"]
    );
    assert_eq!(super::seen_since(&db, 4500).await.unwrap(), ["by-session"]);
    assert!(super::seen_since(&db, 5000).await.unwrap().is_empty());
}
