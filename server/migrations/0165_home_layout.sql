-- Each person's home layout: the widgets they keep, where, and which ones
-- they hide. No row means the built-in order. `version` is the layout's own
-- schema version so a later shape can migrate the rows.

CREATE TABLE app_home_layouts (
    user_id TEXT NOT NULL PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    widgets TEXT NOT NULL DEFAULT '[]',
    hidden TEXT NOT NULL DEFAULT '[]',
    updated_at INTEGER NOT NULL
) STRICT;

-- Seed the hidden set once from the section toggles people already set, so a
-- section they turned off does not come back as a widget. The genre shelf is
-- the grid widget; every other home section is a shelf widget.
INSERT INTO app_home_layouts (user_id, version, widgets, hidden, updated_at)
SELECT p.user_id,
       1,
       '[]',
       json_group_array(
           CASE p.section_key
               WHEN 'genre_list' THEN 'genre_grid'
               ELSE 'shelf:' || p.section_key
           END
           ORDER BY p.section_key
       ),
       CAST(strftime('%s', 'now') AS INTEGER) * 1000
FROM app_section_prefs p
JOIN auth_users u ON u.id = p.user_id
WHERE p.page = 'home' AND p.enabled = 0
GROUP BY p.user_id;

PRAGMA user_version = 165;
