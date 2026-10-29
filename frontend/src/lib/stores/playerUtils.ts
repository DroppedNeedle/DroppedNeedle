import type { NowPlaying, QueueItem, QueueOrigin } from '$lib/player/types';
import { playbackToast } from '$lib/stores/playbackToast.svelte';

const VOLUME_STORAGE_KEY = 'droppedneedle_player_volume';
const SESSION_STORAGE_KEY = 'droppedneedle_player_session_v3';
// The old unversioned save held v2 stream addresses and no owner: it is dropped, never resumed.
const LEGACY_SESSION_STORAGE_KEY = 'droppedneedle_player_session';

export type StoredSession = {
	userId: string;
	nowPlaying: NowPlaying;
	queue: QueueItem[];
	currentIndex: number;
	progress: number;
	shuffleEnabled: boolean;
	shuffleOrder: number[];
};

export function shuffleArray(length: number): number[] {
	const arr = Array.from({ length }, (_, i) => i);
	for (let i = arr.length - 1; i > 0; i--) {
		const j = Math.floor(Math.random() * (i + 1));
		[arr[i], arr[j]] = [arr[j], arr[i]];
	}
	return arr;
}

export function stampOrigin(items: QueueItem[], origin: QueueOrigin): QueueItem[] {
	return items.map((item) => ({ ...item, queueOrigin: origin }));
}

export function stampSingleOrigin(item: QueueItem, origin: QueueOrigin): QueueItem {
	return { ...item, queueOrigin: origin };
}

// The listening room manages its queue inline, so its "added to queue" toasts are noise there.
let queueMutationToastsSuppressed = false;

export function setQueueMutationToastsSuppressed(suppressed: boolean): void {
	queueMutationToastsSuppressed = suppressed;
}

export function showQueueMutationToast(action: 'queue' | 'next', count: number): void {
	if (queueMutationToastsSuppressed) return;
	const label = count === 1 ? 'track' : 'tracks';
	if (action === 'queue') {
		playbackToast.show(
			count === 1 ? 'Added track to queue' : `Added ${count} ${label} to queue`,
			'info'
		);
		return;
	}
	playbackToast.show(
		count === 1 ? 'Queued track to play next' : `Queued ${count} ${label} to play next`,
		'info'
	);
}

export function getStoredVolume(): number {
	try {
		const stored = localStorage.getItem(VOLUME_STORAGE_KEY);
		if (stored !== null) return Math.max(0, Math.min(100, Number(stored)));
	} catch {
		/* empty */
	}
	return 75;
}

export function storeVolume(volume: number): void {
	try {
		localStorage.setItem(VOLUME_STORAGE_KEY, String(volume));
	} catch {
		/* empty */
	}
}

/** The saved session, only when it belongs to `userId`. Any other save is removed. */
export function getStoredSession(userId: string | undefined): StoredSession | null {
	try {
		localStorage.removeItem(LEGACY_SESSION_STORAGE_KEY);
		if (!userId) return null;
		const stored = localStorage.getItem(SESSION_STORAGE_KEY);
		if (!stored) return null;
		const parsed = JSON.parse(stored);
		if (parsed?.userId === userId && parsed.nowPlaying) return parsed as StoredSession;
		localStorage.removeItem(SESSION_STORAGE_KEY);
	} catch {
		/* empty */
	}
	return null;
}

export function storeSessionData(data: StoredSession | null): void {
	try {
		if (data) {
			localStorage.setItem(SESSION_STORAGE_KEY, JSON.stringify(data));
		} else {
			localStorage.removeItem(SESSION_STORAGE_KEY);
			localStorage.removeItem(LEGACY_SESSION_STORAGE_KEY);
		}
	} catch {
		/* empty */
	}
}
