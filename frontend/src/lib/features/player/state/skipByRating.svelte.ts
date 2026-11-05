// The player's rating floor. Off plays everything; 2 to 5 makes Next,
// Previous and a track's end pass over library tracks the listener rated
// below it, and over unrated ones when "Play unrated" is off. A track with
// no library copy can't be rated and always plays. Kept per user in this
// browser.
import type { QueueItem } from '$lib/features/player/playback/types';
import type { TrackRatings } from '$lib/features/library/queries/ratings.svelte';
import { authStore } from '$lib/stores/authStore.svelte';

export interface RatingFloor {
	/** 0 is off, else the fewest stars a track needs to play. */
	minStars: number;
	playUnrated: boolean;
}

export const FLOOR_CHOICES = [2, 3, 4, 5] as const;

const OFF: RatingFloor = { minStars: 0, playUnrated: true };

const storageKey = (userId: string) => `droppedneedle_player_rating_floor:${userId}`;

// Each user's stored floor, read once; the queue asks for it on every row.
const storedFloors: Record<string, RatingFloor> = {};

function parseStored(userId: string): RatingFloor {
	try {
		const parsed = JSON.parse(localStorage.getItem(storageKey(userId)) ?? 'null');
		const minStars = Number(parsed?.minStars);
		const playUnrated = parsed?.playUnrated !== false;
		const valid = (FLOOR_CHOICES as readonly number[]).includes(minStars);
		return { minStars: valid ? minStars : 0, playUnrated };
	} catch {
		return OFF;
	}
}

function readStored(userId: string): RatingFloor {
	storedFloors[userId] ??= parseStored(userId);
	return storedFloors[userId];
}

function store(userId: string, floor: RatingFloor): void {
	try {
		localStorage.setItem(storageKey(userId), JSON.stringify(floor));
	} catch {
		/* the floor still holds for this visit */
	}
}

/** The library track id of a queue item: its own, or its library copy's. */
export function libraryTrackId(item: QueueItem | undefined): string | null {
	if (!item) return null;
	return item.sourceIds?.local ?? (item.sourceType === 'local' ? item.trackSourceId : null);
}

/**
 * The stars that make the floor pass over `item`, or null when it plays.
 * Unrated is 0. A track whose rating isn't read yet plays.
 */
export function skippedRating(
	floor: RatingFloor,
	ratings: TrackRatings | null,
	item: QueueItem | undefined
): number | null {
	if (floor.minStars === 0 || ratings === null) return null;
	const id = libraryTrackId(item);
	if (id === null) return null;
	const stars = ratings.get(id);
	if (stars === undefined) return null;
	if (stars === 0) return floor.playUnrated ? null : 0;
	return stars < floor.minStars ? stars : null;
}

class SkipByRating {
	// What was set this visit, by user; the rest is read from storage.
	#set = $state<Record<string, RatingFloor>>({});

	get floor(): RatingFloor {
		const userId = authStore.user?.id;
		if (!userId) return OFF;
		return this.#set[userId] ?? readStored(userId);
	}

	get on(): boolean {
		return this.floor.minStars > 0;
	}

	#save(floor: RatingFloor): void {
		const userId = authStore.user?.id;
		if (!userId) return;
		this.#set = { ...this.#set, [userId]: floor };
		store(userId, floor);
	}

	setMinStars(minStars: number): void {
		this.#save({ ...this.floor, minStars });
	}

	setPlayUnrated(playUnrated: boolean): void {
		this.#save({ ...this.floor, playUnrated });
	}

	turnOff(): void {
		this.#save({ ...this.floor, minStars: 0 });
	}
}

export const skipByRating = new SkipByRating();
