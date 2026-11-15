-- The two sentinels the baseline seeds, restated so the test says what it
-- reads: Various Artists and Unknown Artist, each credited with albums.
INSERT OR IGNORE INTO local_artists (id, display_name, sort_name, folded_name, normalized_name, kind, created_at, updated_at) VALUES
    ('00000000-0000-4000-8000-000000000001', 'Various Artists', NULL, 'various artists', 'various artists', 'various_artists', 0, 0),
    ('00000000-0000-4000-8000-000000000002', 'Unknown Artist', NULL, 'unknown artist', 'unknown artist', 'unknown', 0, 0);

-- Rows tags keyed for the same placeholders under their own ids: two
-- spellings of "Various Artists" (a differing sort name keys a second row),
-- the abbreviation "Various", and the plural "Unknown Artists".
INSERT INTO local_artists (id, display_name, sort_name, folded_name, normalized_name, kind, created_at, updated_at) VALUES
    ('va-tag-1', 'Various Artists', NULL, 'various artists', 'various artists', 'group', 10, 10),
    ('va-tag-2', 'Various Artists', 'Various Artists', 'various artists', 'various artists', 'group', 11, 11),
    ('various-tag', 'Various', NULL, 'various', 'various', 'group', 12, 12),
    ('unknown-artists', 'Unknown Artists', NULL, 'unknown artists', 'unknown artists', 'group', 13, 13);

-- Two real artists. Boreal's albums were keyed to two rows of one folded
-- name, as a retag that changed only capitals leaves them.
INSERT INTO local_artists (id, display_name, sort_name, folded_name, normalized_name, kind, created_at, updated_at) VALUES
    ('aurora', 'Aurora', NULL, 'aurora', 'aurora', 'group', 14, 14),
    ('boreal', 'Boreal', NULL, 'boreal', 'boreal', 'group', 15, 15),
    ('boreal-upper', 'BOREAL', 'BOREAL', 'boreal', 'boreal', 'group', 16, 16);

INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, created_at, updated_at, year) VALUES
    ('s1', 'r1', 's1', 'Sentinel One', 'sentinel one', 'Various Artists', 'various artists', '00000000-0000-4000-8000-000000000001', 'automatic', 100, 100, 1990),
    ('s2', 'r1', 's2', 'Sentinel Two', 'sentinel two', 'Various Artists', 'various artists', '00000000-0000-4000-8000-000000000001', 'automatic', 101, 101, 1991),
    ('s3', 'r1', 's3', 'Sentinel Three', 'sentinel three', 'Various Artists', 'various artists', '00000000-0000-4000-8000-000000000001', 'automatic', 102, 102, 1992),
    ('u1', 'r1', 'u1', 'Unknown One', 'unknown one', 'Unknown Artist', 'unknown artist', '00000000-0000-4000-8000-000000000002', 'automatic', 103, 103, 1993),
    ('u2', 'r1', 'u2', 'Unknown Two', 'unknown two', 'Unknown Artist', 'unknown artist', '00000000-0000-4000-8000-000000000002', 'automatic', 104, 104, 1994),
    ('v1', 'r1', 'v1', 'Compilation One', 'compilation one', 'Various Artists', 'various artists', 'va-tag-1', 'automatic', 105, 105, 1995),
    ('v2', 'r1', 'v2', 'Compilation Two', 'compilation two', 'Various Artists', 'various artists', 'va-tag-1', 'automatic', 106, 106, 1996),
    ('v3', 'r1', 'v3', 'Compilation Three', 'compilation three', 'Various Artists', 'various artists', 'va-tag-2', 'automatic', 107, 107, 1997),
    ('w1', 'r1', 'w1', 'Varied', 'varied', 'Various', 'various', 'various-tag', 'automatic', 108, 108, 1998),
    ('n1', 'r1', 'n1', 'Nameless One', 'nameless one', 'Unknown Artists', 'unknown artists', 'unknown-artists', 'automatic', 109, 109, 1999),
    ('a1', 'r1', 'a1', 'First Light', 'first light', 'Aurora', 'aurora', 'aurora', 'automatic', 110, 110, 2000),
    ('a2', 'r1', 'a2', 'Second Dawn', 'second dawn', 'Aurora', 'aurora', 'aurora', 'automatic', 111, 111, 2001),
    ('a3', 'r1', 'a3', 'Unindexed', 'unindexed', 'Aurora', 'aurora', 'aurora', 'automatic', 112, 112, 2002),
    ('b1', 'r1', 'b1', 'Lone Peak', 'lone peak', 'Boreal', 'boreal', 'boreal', 'automatic', 113, 113, 2003),
    ('b2', 'r1', 'b2', 'Cold Front', 'cold front', 'Boreal', 'boreal', 'boreal', 'automatic', 114, 114, 2004),
    ('b3', 'r1', 'b3', 'Still Air', 'still air', 'Boreal', 'boreal', 'boreal', 'automatic', 115, 115, 2005),
    ('b4', 'r1', 'b4', 'Night Watch', 'night watch', 'BOREAL', 'boreal', 'boreal-upper', 'automatic', 116, 116, 2006);

INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) VALUES
    ('s1', 0, '00000000-0000-4000-8000-000000000001', 'main'),
    ('s2', 0, '00000000-0000-4000-8000-000000000001', 'main'),
    ('s3', 0, '00000000-0000-4000-8000-000000000001', 'main'),
    ('u1', 0, '00000000-0000-4000-8000-000000000002', 'main'),
    ('u2', 0, '00000000-0000-4000-8000-000000000002', 'main'),
    ('v1', 0, 'va-tag-1', 'main'),
    ('v2', 0, 'va-tag-1', 'main'),
    ('v3', 0, 'va-tag-2', 'main'),
    ('w1', 0, 'various-tag', 'main'),
    ('n1', 0, 'unknown-artists', 'main'),
    ('a1', 0, 'aurora', 'main'),
    ('a2', 0, 'aurora', 'main'),
    ('a3', 0, 'aurora', 'main'),
    ('b1', 0, 'boreal', 'main'),
    ('b2', 0, 'boreal', 'main'),
    ('b3', 0, 'boreal', 'main'),
    ('b4', 0, 'boreal-upper', 'main');

-- One indexed track per album, but for a3: an album no indexed track reaches
-- credits no one.
INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, album_title, album_title_folded, file_format, availability, ingest_source, imported_at, membership_source) VALUES
    ('ts1', 's1', 'r1', '/m/ts1.flac', 'ts1.flac', 'hs1', 1, 1, 'q1', 'One', 'one', 'Sentinel One', 'sentinel one', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('ts2', 's2', 'r1', '/m/ts2.flac', 'ts2.flac', 'hs2', 1, 1, 'q2', 'Two', 'two', 'Sentinel Two', 'sentinel two', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('ts3', 's3', 'r1', '/m/ts3.flac', 'ts3.flac', 'hs3', 1, 1, 'q3', 'Three', 'three', 'Sentinel Three', 'sentinel three', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('tu1', 'u1', 'r1', '/m/tu1.flac', 'tu1.flac', 'hu1', 1, 1, 'q4', 'Four', 'four', 'Unknown One', 'unknown one', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('tu2', 'u2', 'r1', '/m/tu2.flac', 'tu2.flac', 'hu2', 1, 1, 'q5', 'Five', 'five', 'Unknown Two', 'unknown two', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('tv1', 'v1', 'r1', '/m/tv1.flac', 'tv1.flac', 'hv1', 1, 1, 'q6', 'Six', 'six', 'Compilation One', 'compilation one', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('tv2', 'v2', 'r1', '/m/tv2.flac', 'tv2.flac', 'hv2', 1, 1, 'q7', 'Seven', 'seven', 'Compilation Two', 'compilation two', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('tv3', 'v3', 'r1', '/m/tv3.flac', 'tv3.flac', 'hv3', 1, 1, 'q8', 'Eight', 'eight', 'Compilation Three', 'compilation three', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('tw1', 'w1', 'r1', '/m/tw1.flac', 'tw1.flac', 'hw1', 1, 1, 'q9', 'Nine', 'nine', 'Varied', 'varied', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('tn1', 'n1', 'r1', '/m/tn1.flac', 'tn1.flac', 'hn1', 1, 1, 'q10', 'Ten', 'ten', 'Nameless One', 'nameless one', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('ta1', 'a1', 'r1', '/m/ta1.flac', 'ta1.flac', 'ha1', 1, 1, 'q11', 'Eleven', 'eleven', 'First Light', 'first light', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('ta2', 'a2', 'r1', '/m/ta2.flac', 'ta2.flac', 'ha2', 1, 1, 'q12', 'Twelve', 'twelve', 'Second Dawn', 'second dawn', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('ta3', 'a3', 'r1', '/m/ta3.flac', 'ta3.flac', 'ha3', 1, 1, 'q13', 'Thirteen', 'thirteen', 'Unindexed', 'unindexed', 'flac', 'missing', 'scan', 1, 'automatic'),
    ('tb1', 'b1', 'r1', '/m/tb1.flac', 'tb1.flac', 'hb1', 1, 1, 'q14', 'Fourteen', 'fourteen', 'Lone Peak', 'lone peak', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('tb2', 'b2', 'r1', '/m/tb2.flac', 'tb2.flac', 'hb2', 1, 1, 'q15', 'Fifteen', 'fifteen', 'Cold Front', 'cold front', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('tb3', 'b3', 'r1', '/m/tb3.flac', 'tb3.flac', 'hb3', 1, 1, 'q16', 'Sixteen', 'sixteen', 'Still Air', 'still air', 'flac', 'indexed', 'scan', 1, 'automatic'),
    ('tb4', 'b4', 'r1', '/m/tb4.flac', 'tb4.flac', 'hb4', 1, 1, 'q17', 'Seventeen', 'seventeen', 'Night Watch', 'night watch', 'flac', 'indexed', 'scan', 1, 'automatic');
