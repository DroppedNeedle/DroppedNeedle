INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
VALUES ('ar-ess', 'Essentials Artist', 'essentials artist', 'group', 1, 1);

INSERT INTO local_artist_external_identities (local_artist_id, provider, provider_artist_id, decision_source, selected_at)
VALUES ('ar-ess', 'musicbrainz', 'ess-artist', 'manual', 1);

WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 501)
INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, created_at, updated_at, has_indexed_track)
SELECT printf('al-ess-%03d', i), 'root', printf('k-ess-%03d', i), printf('Owned %03d', i), printf('owned %03d', i), 'Essentials Artist', 'essentials artist', 'ar-ess', 'automatic', 1000, 1000 + i, 1 FROM n;

INSERT INTO local_album_external_identities (local_album_id, provider, release_group_mbid, decision_source, selected_at)
SELECT id, 'musicbrainz', 'rg-' || id, 'manual', 1 FROM local_albums WHERE id LIKE 'al-ess-%';

INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, created_at, updated_at, has_indexed_track)
VALUES ('al-tagged', 'root', 'k-tagged', 'Tagged Only', 'tagged only', 'Essentials Artist', 'essentials artist', 'ar-ess', 'automatic', 2000, 2000, 1);
