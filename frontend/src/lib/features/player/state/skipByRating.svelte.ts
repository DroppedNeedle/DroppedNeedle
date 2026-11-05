// The player's rating floor: 0 plays everything; 1 to 5 makes next and
// previous pass over library tracks the listener rated below it, unrated
// ones too. Tracks from other sources can't be rated and always play, and
// so does every track while the ratings are not read yet. The track a
// person starts plays whatever its rating. Kept in the browser, so it
// outlives a reload.
import type { QueueItem } from '$lib/features/player/playback/types';

const KEY = 'player.skipBelowStars';

function read(): number {
	if (typeof localStorage === 'undefined') return 0;
	const stored = Number(localStorage.getItem(KEY));
	return Number.isInteger(stored) && stored >= 1 && stored <= 5 ? stored : 0;
}

/** The library track id of a queue item, when it has one. */
export function libraryTrackId(item: QueueItem | undefined): string | null {
	if (!item) return null;
	return item.sourceIds?.local ?? (item.sourceType === 'local' ? item.trackSourceId : null);
}

class SkipByRating {
	minStars = $state(read());
	/** The listener's track ratings by library track id; null until read. */
	ratings = $state<Record<string, number> | null>(null);

	set(stars: number): void {
		this.minStars = stars;
		if (typeof localStorage === 'undefined') return;
		if (stars > 0) localStorage.setItem(KEY, String(stars));
		else localStorage.removeItem(KEY);
	}

	/** Whether next and previous may land on `item`. */
	allows(item: QueueItem | undefined): boolean {
		if (this.minStars === 0 || this.ratings === null) return true;
		const id = libraryTrackId(item);
		if (id === null) return true;
		return (this.ratings[id] ?? 0) >= this.minStars;
	}
}

export const skipByRating = new SkipByRating();
