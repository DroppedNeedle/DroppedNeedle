import type { QueueItem } from '$lib/features/player/playback/types';
import { stampOrigin, stampSingleOrigin } from './playerUtils';
import { reorderItems, reorderShuffleItems } from './playerQueueOps';

export function addItemToQueue(
	queue: QueueItem[],
	item: QueueItem,
	shuffleEnabled: boolean,
	shuffleOrder: number[]
): { newQueue: QueueItem[]; newShuffleOrder: number[] } {
	const newQueue = [...queue, stampSingleOrigin(item, 'manual')];
	const newShuffleOrder = shuffleEnabled ? [...shuffleOrder, newQueue.length - 1] : shuffleOrder;
	return { newQueue, newShuffleOrder };
}

export function addMultipleItems(
	queue: QueueItem[],
	items: QueueItem[],
	shuffleEnabled: boolean,
	shuffleOrder: number[]
): { newQueue: QueueItem[]; newShuffleOrder: number[] } {
	const stamped = stampOrigin(items, 'manual');
	const startIdx = queue.length;
	const newQueue = [...queue, ...stamped];
	const newShuffleOrder = shuffleEnabled
		? [...shuffleOrder, ...items.map((_, i) => startIdx + i)]
		: shuffleOrder;
	return { newQueue, newShuffleOrder };
}

export function insertPlayNext(
	queue: QueueItem[],
	item: QueueItem,
	currentIndex: number,
	shuffleEnabled: boolean,
	shuffleOrder: number[]
): { newQueue: QueueItem[]; newShuffleOrder: number[] } {
	const insertAt = currentIndex + 1;
	const newQueue = [...queue];
	newQueue.splice(insertAt, 0, stampSingleOrigin(item, 'manual'));
	let newShuffleOrder = shuffleOrder;
	if (shuffleEnabled) {
		const updated = shuffleOrder.map((i) => (i >= insertAt ? i + 1 : i));
		const shuffleIdx = updated.indexOf(currentIndex);
		updated.splice(shuffleIdx + 1, 0, insertAt);
		newShuffleOrder = updated;
	}
	return { newQueue, newShuffleOrder };
}

export function insertMultipleNext(
	queue: QueueItem[],
	items: QueueItem[],
	currentIndex: number,
	shuffleEnabled: boolean,
	shuffleOrder: number[]
): { newQueue: QueueItem[]; newShuffleOrder: number[] } {
	const stamped = stampOrigin(items, 'manual');
	const insertAt = currentIndex + 1;
	const newQueue = [...queue];
	newQueue.splice(insertAt, 0, ...stamped);
	let newShuffleOrder = shuffleOrder;
	if (shuffleEnabled) {
		const updated = shuffleOrder.map((i) => (i >= insertAt ? i + items.length : i));
		const shuffleIdx = updated.indexOf(currentIndex);
		const newIndices = items.map((_, i) => insertAt + i);
		updated.splice(shuffleIdx + 1, 0, ...newIndices);
		newShuffleOrder = updated;
	}
	return { newQueue, newShuffleOrder };
}

export function removeAtIndex(
	queue: QueueItem[],
	index: number,
	currentIndex: number,
	shuffleEnabled: boolean,
	shuffleOrder: number[]
): { newQueue: QueueItem[]; newIndex: number; newShuffleOrder: number[]; wasPlaying: boolean } {
	const wasPlaying = index === currentIndex;
	const newQueue = queue.filter((_, i) => i !== index);
	const newShuffleOrder = shuffleEnabled
		? shuffleOrder.filter((i) => i !== index).map((i) => (i > index ? i - 1 : i))
		: shuffleOrder;
	const newIndex = wasPlaying
		? Math.min(index, newQueue.length - 1)
		: currentIndex > index
			? currentIndex - 1
			: currentIndex;
	return { newQueue, newIndex, newShuffleOrder, wasPlaying };
}

export function performReorder(
	queue: QueueItem[],
	fromIndex: number,
	toIndex: number,
	currentIndex: number
): { newQueue: QueueItem[]; newCurrentIndex: number } {
	if (
		fromIndex === toIndex ||
		fromIndex < 0 ||
		fromIndex >= queue.length ||
		toIndex < 0 ||
		toIndex >= queue.length
	) {
		return { newQueue: queue, newCurrentIndex: currentIndex };
	}
	const newQueue = reorderItems(queue, fromIndex, toIndex);
	let newCurrentIndex = currentIndex;
	if (currentIndex === fromIndex) {
		newCurrentIndex = toIndex;
	} else if (fromIndex < currentIndex && toIndex >= currentIndex) {
		newCurrentIndex = currentIndex - 1;
	} else if (fromIndex > currentIndex && toIndex <= currentIndex) {
		newCurrentIndex = currentIndex + 1;
	}
	return { newQueue, newCurrentIndex };
}

export function performShuffleReorder(
	shuffleOrder: number[],
	fromPos: number,
	toPos: number
): number[] {
	return reorderShuffleItems(shuffleOrder, fromPos, toPos);
}

export function clearQueueKeepCurrent(
	queue: QueueItem[],
	currentIndex: number
): { newQueue: QueueItem[]; newIndex: number } {
	const currentItem = queue[currentIndex];
	if (!currentItem) return { newQueue: [], newIndex: 0 };
	return { newQueue: [currentItem], newIndex: 0 };
}
