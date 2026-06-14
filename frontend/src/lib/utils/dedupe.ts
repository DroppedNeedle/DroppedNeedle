/**
 * Remove entries with a duplicate or missing `id`, preserving first-seen order.
 *
 * Lists that are accumulated across paginated/infinite-query responses or merged
 * from multiple sources (e.g. Lidarr + MusicBrainz) can contain the same item
 * more than once. When such a list feeds a keyed `{#each ... (item.id)}` block,
 * a duplicate (or `null`/`undefined`) key throws svelte's `each_key_duplicate`
 * and blanks the whole page, so callers should dedupe before rendering.
 */
export function dedupeById<T extends { id?: string | null }>(items: T[]): T[] {
	const seen = new Set<string>();
	const result: T[] = [];
	for (const item of items) {
		if (!item.id || seen.has(item.id)) continue;
		seen.add(item.id);
		result.push(item);
	}
	return result;
}
