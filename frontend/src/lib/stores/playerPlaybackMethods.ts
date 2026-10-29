import type { QueueItem, SourceType } from '$lib/player/types';
import { shuffleArray, stampOrigin } from './playerUtils';

export interface PlayQueueResult {
	queue: QueueItem[];
	shuffleEnabled: boolean;
	shuffleOrder: number[];
	isPlayerVisible: boolean;
	startIndex: number;
}

export function buildPlayQueueState(
	items: QueueItem[],
	startIndex: number,
	shuffle: boolean
): PlayQueueResult {
	const queue = stampOrigin(items, 'context');
	const shuffleOrder = shuffle ? shuffleArray(items.length) : [];
	const actualStart = shuffle ? shuffleOrder[0] : startIndex;
	return {
		queue,
		shuffleEnabled: shuffle,
		shuffleOrder,
		isPlayerVisible: true,
		startIndex: actualStart
	};
}

export interface ToggleShuffleResult {
	shuffleEnabled: boolean;
	shuffleOrder: number[];
}

export function computeToggleShuffle(
	queueLength: number,
	currentIndex: number,
	currentlyEnabled: boolean
): ToggleShuffleResult {
	if (!currentlyEnabled) {
		const allIndices = Array.from({ length: queueLength }, (_, i) => i);
		const upcoming = allIndices.filter((i) => i !== currentIndex && i > currentIndex);
		const played = allIndices.filter((i) => i < currentIndex);

		for (let i = upcoming.length - 1; i > 0; i--) {
			const j = Math.floor(Math.random() * (i + 1));
			[upcoming[i], upcoming[j]] = [upcoming[j], upcoming[i]];
		}

		return {
			shuffleEnabled: true,
			shuffleOrder: [...played, currentIndex, ...upcoming]
		};
	}
	return { shuffleEnabled: false, shuffleOrder: [] };
}

export function changeItemSource(
	queue: QueueItem[],
	index: number,
	newSourceType: SourceType
): { newQueue: QueueItem[]; error?: string } {
	if (index < 0 || index >= queue.length) return { newQueue: queue };

	const item = queue[index];
	if (!item.availableSources?.includes(newSourceType)) return { newQueue: queue };

	const resolvedId = item.sourceIds?.[newSourceType];
	if (!resolvedId) return { newQueue: queue, error: 'Source ID unavailable for this track' };

	const newQueue = [...queue];
	newQueue[index] = {
		...item,
		sourceType: newSourceType,
		trackSourceId: resolvedId
	};
	return { newQueue };
}

export function updateItemByPlaylistTrackId(
	queue: QueueItem[],
	playlistTrackId: string,
	currentIndex: number,
	newSourceType: SourceType,
	newTrackSourceId: string,
	newFormat?: string,
	plexRatingKey?: string
): QueueItem[] | null {
	const index = queue.findIndex((item) => item.playlistTrackId === playlistTrackId);
	if (index < 0 || index === currentIndex) return null;

	const newQueue = [...queue];
	newQueue[index] = {
		...newQueue[index],
		sourceType: newSourceType,
		trackSourceId: newTrackSourceId,
		format: newFormat,
		plexRatingKey: newSourceType === 'plex' ? plexRatingKey : undefined
	};
	return newQueue;
}
