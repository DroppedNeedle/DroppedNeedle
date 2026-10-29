import type { NowPlaying, QueueItem } from '$lib/player/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { getStoredSession, storeSessionData } from './playerUtils';
import type { StoredSession } from './playerUtils';

/** Saves the session under the signed-in user; with nobody signed in nothing is kept. */
export function persistSession(
	nowPlaying: NowPlaying | null,
	queue: QueueItem[],
	currentIndex: number,
	progress: number,
	shuffleEnabled: boolean,
	shuffleOrder: number[]
): void {
	const userId = authStore.user?.id;
	if (!nowPlaying || !userId) {
		storeSessionData(null);
		return;
	}
	storeSessionData({
		userId,
		nowPlaying,
		queue,
		currentIndex,
		progress,
		shuffleEnabled,
		shuffleOrder
	});
}

export function restoreSessionData(): StoredSession | null {
	return getStoredSession(authStore.user?.id);
}

export interface ResumeState {
	nowPlaying: NowPlaying;
	queue: QueueItem[];
	currentIndex: number;
	progress: number;
	shuffleEnabled: boolean;
	shuffleOrder: number[];
	currentItem: QueueItem;
}

export function buildResumeState(session: StoredSession): ResumeState | null {
	if (session.nowPlaying.sourceType === 'youtube') return null;
	const queue = session.queue.map((item) => ({
		...item,
		queueOrigin: item.queueOrigin ?? 'context'
	}));
	const currentItem = queue[session.currentIndex];
	if (!currentItem) return null;

	return {
		nowPlaying: session.nowPlaying,
		queue,
		currentIndex: session.currentIndex,
		progress: session.progress,
		shuffleEnabled: session.shuffleEnabled,
		shuffleOrder: session.shuffleOrder,
		currentItem
	};
}
