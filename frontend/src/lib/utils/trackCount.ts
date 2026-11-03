/**
 * How many tracks the library holds of an album: "4/12 tracks" while
 * tracks of the album's edition are missing, else "12 tracks".
 */
export function trackCountLabel(held: number, expected?: number | null): string {
	if (expected && held < expected)
		return `${held}/${expected} ${expected === 1 ? 'track' : 'tracks'}`;
	return `${held} ${held === 1 ? 'track' : 'tracks'}`;
}
