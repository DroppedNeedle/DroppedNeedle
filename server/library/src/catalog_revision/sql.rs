//! The catalog revision's statements.

droppedneedle_kernel::sql! {
    /// Move the catalog revision by one and return the new value.
    pub(super) BUMP = "INSERT INTO library_catalog_revision (singleton, value) VALUES (1, 1) \
             ON CONFLICT (singleton) DO UPDATE SET value = value + 1 RETURNING value";
    /// Raise the catalog revision to at least `?1`, never lowering it.
    pub(super) RAISE = "INSERT INTO library_catalog_revision (singleton, value) VALUES (1, ?1) \
             ON CONFLICT (singleton) DO UPDATE SET value = MAX(value, excluded.value)";
}
