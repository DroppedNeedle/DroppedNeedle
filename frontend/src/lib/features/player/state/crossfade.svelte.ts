// What the web player does between songs. Crossfade is off (0) or 2 to 12
// seconds; "inside albums" decides whether songs that follow each other on
// one album blend too, else they play straight through; "even out volume"
// plays every song at a similar loudness. Kept per user in this browser.
import { authStore } from '$lib/stores/authStore.svelte';

export interface BetweenSongs {
	/** 0 is off, else how long one song blends into the next. */
	seconds: number;
	insideAlbums: boolean;
	evenOutVolume: boolean;
}

export const CROSSFADE_MAX_S = 12;
export const CROSSFADE_STEP_S = 2;

const DEFAULTS: BetweenSongs = { seconds: 0, insideAlbums: false, evenOutVolume: true };

const storageKey = (userId: string) => `droppedneedle_player_crossfade:${userId}`;

function isChoice(seconds: number): boolean {
	return (
		Number.isInteger(seconds) &&
		seconds >= 0 &&
		seconds <= CROSSFADE_MAX_S &&
		seconds % CROSSFADE_STEP_S === 0
	);
}

// Each user's stored settings, read once; the player asks a few times a second.
const storedSettings: Record<string, BetweenSongs> = {};

function readStored(userId: string): BetweenSongs {
	storedSettings[userId] ??= parseStored(userId);
	return storedSettings[userId];
}

function parseStored(userId: string): BetweenSongs {
	try {
		const parsed = JSON.parse(localStorage.getItem(storageKey(userId)) ?? 'null');
		const seconds = Number(parsed?.seconds);
		return {
			seconds: isChoice(seconds) ? seconds : DEFAULTS.seconds,
			insideAlbums: parsed?.insideAlbums === true,
			evenOutVolume: parsed?.evenOutVolume !== false
		};
	} catch {
		return DEFAULTS;
	}
}

function store(userId: string, settings: BetweenSongs): void {
	try {
		localStorage.setItem(storageKey(userId), JSON.stringify(settings));
	} catch {
		/* the setting still holds for this visit */
	}
}

class Crossfade {
	// What was set or read this visit, by user.
	#known = $state<Record<string, BetweenSongs>>({});

	get settings(): BetweenSongs {
		const userId = authStore.user?.id;
		if (!userId) return DEFAULTS;
		return this.#known[userId] ?? readStored(userId);
	}

	get seconds(): number {
		return this.settings.seconds;
	}

	get insideAlbums(): boolean {
		return this.settings.insideAlbums;
	}

	get evenOutVolume(): boolean {
		return this.settings.evenOutVolume;
	}

	#save(change: Partial<BetweenSongs>): void {
		const userId = authStore.user?.id;
		if (!userId) return;
		const settings = { ...this.settings, ...change };
		this.#known = { ...this.#known, [userId]: settings };
		store(userId, settings);
	}

	setSeconds(seconds: number): void {
		if (isChoice(seconds)) this.#save({ seconds });
	}

	setInsideAlbums(insideAlbums: boolean): void {
		this.#save({ insideAlbums });
	}

	setEvenOutVolume(evenOutVolume: boolean): void {
		this.#save({ evenOutVolume });
	}
}

export const crossfade = new Crossfade();
