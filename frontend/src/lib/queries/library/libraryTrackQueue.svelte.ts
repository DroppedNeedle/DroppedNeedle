import type { QueueItem } from '$lib/player/types';
import type { NativeTrackListItem } from '$lib/types';
import type { LibraryV3TracksParams } from './LibraryQueryKeyFactory';
import {
	fetchLibraryTracks,
	libraryTrackParams,
	type LibraryTrackPage,
	type LibraryTrackSort
} from './libraryTracks';

const BATCH_SIZE = 500;

export interface LibraryTrackQueueHooks {
	buildQueue(tracks: NativeTrackListItem[]): QueueItem[];
	playQueue(items: QueueItem[], startIndex: number, shuffle: boolean): void;
	appendQueue(items: QueueItem[]): void;
	reshuffle(): void;
	toast(message: string, type: 'info' | 'error'): void;
}

/** How a start went: playing, nothing to play, or the first page failed (already said). */
export type LibraryTrackQueueStart = 'playing' | 'empty' | 'failed';

// Plays the whole local track list: the first page at once, the rest
// appended in the background as the cursor walks on. Shuffle walks the
// list's own shuffle, so every track comes once.
export function createLibraryTrackQueue(hooks: LibraryTrackQueueHooks) {
	let controller: AbortController | null = null;
	let loading = $state(false);
	let loaded = $state(0);
	let total = $state(0);

	function release(ac: AbortController) {
		if (controller === ac) {
			loading = false;
			controller = null;
		}
	}

	function abort() {
		controller?.abort();
		controller = null;
		loading = false;
	}

	async function walk(
		ac: AbortController,
		params: LibraryV3TracksParams,
		after: string,
		shuffle: boolean
	) {
		let next: string | null = after;
		try {
			while (next !== null) {
				const page = await fetchLibraryTracks(params, next, ac.signal);
				if (ac.signal.aborted) return;
				const batch = hooks.buildQueue(page.items);
				if (batch.length > 0) {
					hooks.appendQueue(batch);
					if (shuffle) hooks.reshuffle();
				}
				loaded += page.items.length;
				next = page.next;
			}
		} catch {
			if (!ac.signal.aborted) {
				hooks.toast(
					`Couldn't load the rest of the tracks. ${loaded} of ${total} are ready.`,
					'error'
				);
			}
		} finally {
			release(ac);
		}
	}

	// Plays from `first` (or the list's first page), then walks the rest.
	async function play(
		sort: LibraryTrackSort,
		q: string,
		shuffle: boolean,
		first: LibraryTrackPage | null
	): Promise<LibraryTrackQueueStart> {
		abort();
		const ac = new AbortController();
		controller = ac;
		const params = libraryTrackParams(BATCH_SIZE, sort, q);
		let page: LibraryTrackPage;
		try {
			page = first ?? (await fetchLibraryTracks(params, null, ac.signal));
		} catch {
			release(ac);
			if (ac.signal.aborted) return 'empty';
			hooks.toast("Couldn't start playback", 'error');
			return 'failed';
		}
		const queue = hooks.buildQueue(page.items);
		if (ac.signal.aborted || queue.length === 0) {
			release(ac);
			return 'empty';
		}
		hooks.playQueue(queue, 0, shuffle);
		loaded = page.items.length;
		total = page.total;
		if (page.next === null) {
			release(ac);
		} else {
			loading = true;
			void walk(ac, params, page.next, shuffle);
		}
		return 'playing';
	}

	return {
		get loading() {
			return loading;
		},
		get progressText() {
			return loading ? `Loading ${loaded.toLocaleString()} of ${total.toLocaleString()}` : null;
		},
		/** Plays the list in `sort` order, continuing after `first` when given. */
		playAll: (sort: LibraryTrackSort, q: string, first: LibraryTrackPage | null = null) =>
			play(sort, q, false, first),
		/** Plays the list shuffled. */
		shuffleAll: (q: string) => play('random', q, true, null),
		abort
	};
}
