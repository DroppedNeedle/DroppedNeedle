//! The statements of the settings store: one row per section, and the
//! record of the one-time copy of `config.json`.

crate::sql! {
    /// Every stored section.
    pub ALL_SECTIONS = "SELECT key, value_json, revision FROM config_sections";
    /// One stored section and its revision.
    pub ONE_SECTION = "SELECT value_json, revision FROM config_sections WHERE key = ?1";
    /// Store one section: revision 1 when new, one more than before when
    /// changed. Answers the revision written.
    pub PUT_SECTION = "INSERT INTO config_sections (key, value_json, revision, updated_at)
                 VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT (key) DO UPDATE SET
                     value_json = excluded.value_json,
                     revision = config_sections.revision + 1,
                     updated_at = excluded.updated_at
                 RETURNING revision";
    /// Remove one section, so it reads as its default. Only the offline
    /// import does this: the running server never deletes a row.
    pub DELETE_SECTION = "DELETE FROM config_sections WHERE key = ?1";
    /// Whether any section or the ingest record is stored.
    pub ANY_SETTINGS = "SELECT EXISTS(SELECT 1 FROM config_sections)
                 OR EXISTS(SELECT 1 FROM config_ingests)";
    /// Whether any account exists: a skip marker beside such a database was
    /// written for another one.
    pub(super) ANY_ACCOUNT = "SELECT EXISTS(SELECT 1 FROM auth_users)";
    /// Every stored value that looks sealed under the data key: `v3:` text
    /// in a settings row, in a `user_connections` document (sealed whole,
    /// or plain JSON with sealed fields), an app password or a federated
    /// sign-in's tokens. Keep in step with `forget_secrets` in the server's
    /// `bootstrap/key_check.rs`, which clears the same places. A document
    /// that is not JSON is walked as an empty array, because `json_tree`
    /// fails the whole query on one.
    pub(super) SEALED_VALUES = "SELECT tree.value FROM config_sections,
                     json_tree(config_sections.value_json) AS tree
                 WHERE tree.type = 'text' AND tree.value LIKE 'v3:%'
                 UNION ALL
                 SELECT connection_data FROM user_connections
                 WHERE connection_data LIKE 'v3:%'
                 UNION ALL
                 SELECT tree.value FROM user_connections,
                     json_tree(CASE WHEN json_valid(user_connections.connection_data)
                         THEN user_connections.connection_data ELSE '[]' END) AS tree
                 WHERE tree.type = 'text' AND tree.value LIKE 'v3:%'
                 UNION ALL
                 SELECT secret_encrypted FROM connect_app_passwords
                 WHERE secret_encrypted LIKE 'v3:%'
                 UNION ALL
                 SELECT provider_data FROM auth_providers WHERE provider_data LIKE 'v3:%'";
    /// Record what happened to the `config.json` found at the first start.
    pub(super) RECORD_INGEST = "INSERT INTO config_ingests
                 (singleton, outcome, file_sha256, ingested_at, keys_copied, keys_left)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5)";
}
