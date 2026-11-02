/**
 * How many tracks the library holds of an album: "4/12 tracks" when the
 * album's edition count is known, else "4 tracks".
 */
export function trackCountLabel(held: number, expected?: number | null): string {
	if (expected) return `${held}/${expected} ${expected === 1 ? 'track' : 'tracks'}`;
	return `${held} ${held === 1 ? 'track' : 'tracks'}`;
}

/** What the library holds of one release group, over all its copies. */
export interface HeldTracks {
	held: number;
	expected: number | null;
}

/**
 * The tracks the library holds per release group (lowercase id), from the
 * artist's library albums. Copies of one group add up (an album split over
 * disc folders), up to the edition's count (a second copy holds the same
 * tracks again); albums without a group or without files are left out.
 */
export function heldTracksByGroup(
	albums: {
		release_group_mbid?: string | null;
		track_count: number;
		expected_track_count?: number | null;
	}[]
): Map<string, HeldTracks> {
	const byGroup = new Map<string, HeldTracks>();
	for (const album of albums) {
		const group = album.release_group_mbid?.toLowerCase();
		if (!group || album.track_count < 1) continue;
		const known = byGroup.get(group);
		const expected = album.expected_track_count ?? null;
		byGroup.set(group, {
			held: (known?.held ?? 0) + album.track_count,
			expected: Math.max(known?.expected ?? 0, expected ?? 0) || null
		});
	}
	for (const tracks of byGroup.values()) {
		if (tracks.expected) tracks.held = Math.min(tracks.held, tracks.expected);
	}
	return byGroup;
}
