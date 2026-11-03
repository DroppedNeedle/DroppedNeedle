import type { QueueItem } from '$lib/features/player/playback/types';

export function computeNextIndex(
	currentIndex: number,
	queueLength: number,
	shuffleEnabled: boolean,
	shuffleOrder: number[]
): number | null {
	if (queueLength <= 1) return null;
	if (shuffleEnabled) {
		const shuffleIdx = shuffleOrder.indexOf(currentIndex);
		if (shuffleIdx < shuffleOrder.length - 1) return shuffleOrder[shuffleIdx + 1];
		return null;
	}
	if (currentIndex < queueLength - 1) return currentIndex + 1;
	return null;
}

export function computePreviousIndex(
	currentIndex: number,
	queueLength: number,
	shuffleEnabled: boolean,
	shuffleOrder: number[]
): number | null {
	if (queueLength <= 1) return null;
	if (shuffleEnabled) {
		const shuffleIdx = shuffleOrder.indexOf(currentIndex);
		if (shuffleIdx > 0) return shuffleOrder[shuffleIdx - 1];
		return null;
	}
	if (currentIndex > 0) return currentIndex - 1;
	return null;
}

export function computeUpcomingLength(
	queueLength: number,
	currentIndex: number,
	shuffleEnabled: boolean,
	shuffleOrder: number[]
): number {
	if (queueLength === 0) return 0;
	if (shuffleEnabled) {
		const shuffleIdx = shuffleOrder.indexOf(currentIndex);
		if (shuffleIdx < 0) return Math.max(0, queueLength - 1);
		return Math.max(0, shuffleOrder.length - shuffleIdx - 1);
	}
	return Math.max(0, queueLength - currentIndex - 1);
}

// Album tracks sit in consecutive runs sharing a non-empty albumId; no albumId means a solo block.
function albumKeyAt(queue: QueueItem[], idx: number): string {
	const id = queue[idx]?.albumId;
	return id ? `id:${id}` : `solo:${idx}`;
}

// Gates album-skip controls: only true with >=2 consecutive tracks sharing an albumId.
export function queueHasAlbums(queue: QueueItem[]): boolean {
	for (let i = 1; i < queue.length; i++) {
		const id = queue[i]?.albumId;
		if (id && id === queue[i - 1]?.albumId) return true;
	}
	return false;
}

export function computeNextAlbumIndex(queue: QueueItem[], currentIndex: number): number | null {
	if (currentIndex < 0 || currentIndex >= queue.length) return null;
	const curKey = albumKeyAt(queue, currentIndex);
	for (let i = currentIndex + 1; i < queue.length; i++) {
		if (albumKeyAt(queue, i) !== curKey) return i;
	}
	return null;
}

// Mid-album restarts the current album; at its start jumps to the previous album block.
export function computePreviousAlbumIndex(queue: QueueItem[], currentIndex: number): number | null {
	if (currentIndex < 0 || currentIndex >= queue.length) return null;
	const curKey = albumKeyAt(queue, currentIndex);
	let startOfCurrent = currentIndex;
	while (startOfCurrent > 0 && albumKeyAt(queue, startOfCurrent - 1) === curKey) startOfCurrent--;
	if (currentIndex > startOfCurrent) return startOfCurrent;
	if (startOfCurrent === 0) return null;
	const prevKey = albumKeyAt(queue, startOfCurrent - 1);
	let startOfPrev = startOfCurrent - 1;
	while (startOfPrev > 0 && albumKeyAt(queue, startOfPrev - 1) === prevKey) startOfPrev--;
	return startOfPrev;
}

export function performCleanup(
	queue: QueueItem[],
	currentIndex: number,
	shuffleEnabled: boolean,
	shuffleOrder: number[],
	maxHistory: number
): { newQueue: QueueItem[]; newIndex: number; newShuffleOrder: number[] } {
	if (queue.length <= 1)
		return { newQueue: queue, newIndex: currentIndex, newShuffleOrder: shuffleOrder };

	let playedIndices: number[];
	if (shuffleEnabled) {
		const currentShufflePos = shuffleOrder.indexOf(currentIndex);
		if (currentShufflePos <= 0)
			return { newQueue: queue, newIndex: currentIndex, newShuffleOrder: shuffleOrder };
		playedIndices = shuffleOrder.slice(0, currentShufflePos);
	} else {
		if (currentIndex <= 0)
			return { newQueue: queue, newIndex: currentIndex, newShuffleOrder: shuffleOrder };
		playedIndices = Array.from({ length: currentIndex }, (_, i) => i);
	}

	const toRemove = new Set<number>();
	for (const idx of playedIndices) {
		if (queue[idx]?.queueOrigin === 'manual') toRemove.add(idx);
	}

	const remainingPlayed = playedIndices.filter((idx) => !toRemove.has(idx));
	const excess = remainingPlayed.length - maxHistory;
	if (excess > 0) {
		for (let i = 0; i < excess; i++) toRemove.add(remainingPlayed[i]);
	}

	if (toRemove.size === 0)
		return { newQueue: queue, newIndex: currentIndex, newShuffleOrder: shuffleOrder };

	const indexMap = new Map<number, number>();
	let shift = 0;
	for (let i = 0; i < queue.length; i++) {
		if (toRemove.has(i)) {
			shift++;
		} else {
			indexMap.set(i, i - shift);
		}
	}

	const newQueue = queue.filter((_, i) => !toRemove.has(i));
	const newIndex = indexMap.get(currentIndex) ?? 0;
	const newShuffleOrder = shuffleEnabled
		? shuffleOrder.filter((i) => !toRemove.has(i)).map((i) => indexMap.get(i)!)
		: shuffleOrder;

	return { newQueue, newIndex, newShuffleOrder };
}

export function reorderItems<T>(items: T[], fromIndex: number, toIndex: number): T[] {
	if (fromIndex === toIndex) return items;
	const newItems = [...items];
	const [moved] = newItems.splice(fromIndex, 1);
	newItems.splice(toIndex, 0, moved);
	return newItems;
}

export function reorderShuffleItems(
	shuffleOrder: number[],
	fromPos: number,
	toPos: number
): number[] {
	if (fromPos === toPos) return shuffleOrder;
	const newOrder = [...shuffleOrder];
	const [moved] = newOrder.splice(fromPos, 1);
	newOrder.splice(toPos, 0, moved);
	return newOrder;
}
