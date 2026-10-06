import type { QueueItem } from '$lib/player/types';

const BACKGROUND_BATCH_SIZE = 100;

export interface TrackPageResponse<T> {
	items: T[];
	total: number;
}

export interface LibraryTrackLoaderConfig<T> {
	fetchPage(limit: number, offset: number, signal: AbortSignal): Promise<TrackPageResponse<T>>;
	buildQueue(tracks: T[]): QueueItem[];
	pageSize: number;
	resolveShuffleStartIndex?(tracks: T[], requestedIndex: number, queue: QueueItem[]): number;
}

function shuffleInPlace<T>(arr: T[]): T[] {
	for (let i = arr.length - 1; i > 0; i--) {
		const j = Math.floor(Math.random() * (i + 1));
		[arr[i], arr[j]] = [arr[j], arr[i]];
	}
	return arr;
}

export function createLibraryTrackLoader<T>(
	config: LibraryTrackLoaderConfig<T>,
	onQueueAppend: (items: QueueItem[]) => void,
	onPlayQueue: (items: QueueItem[], startIndex: number, shuffle: boolean) => void,
	onShuffleRegenerate: () => void,
	onToast: (message: string, type: 'info' | 'error') => void
) {
	let controller: AbortController | null = null;
	let loading = $state(false);
	let loaded = $state(0);
	let total = $state(0);

	function abort() {
		controller?.abort();
		controller = null;
		loading = false;
	}

	function playAll(currentItems: T[], currentTotal: number) {
		abort();
		const queue = config.buildQueue(currentItems);
		if (queue.length === 0) return;

		if (currentTotal <= currentItems.length) {
			onPlayQueue(queue, 0, false);
			return;
		}

		onPlayQueue(queue, 0, false);

		const ac = new AbortController();
		controller = ac;
		loading = true;
		loaded = currentItems.length;
		total = currentTotal;

		const snapshotTotal = currentTotal;
		let offset = currentItems.length;

		(async () => {
			try {
				while (offset < snapshotTotal) {
					if (ac.signal.aborted) return;
					const page = await config.fetchPage(BACKGROUND_BATCH_SIZE, offset, ac.signal);
					if (ac.signal.aborted) return;
					if (page.items.length === 0) break;

					const batch = config.buildQueue(page.items);
					if (batch.length > 0) {
						onQueueAppend(batch);
					}
					offset += page.items.length;
					loaded = offset;
				}
				onShuffleRegenerate();
			} catch (_e: unknown) {
				if (ac.signal.aborted) return;
				onToast(
					`Couldn't load the rest of the tracks. ${loaded} of ${snapshotTotal} are ready.`,
					'error'
				);
			} finally {
				if (controller === ac) {
					loading = false;
					controller = null;
				}
			}
		})();
	}

	function shuffleAll(currentItems: T[], currentTotal: number) {
		abort();

		if (currentTotal <= config.pageSize) {
			const queue = config.buildQueue(currentItems);
			if (queue.length > 0) onPlayQueue(queue, 0, true);
			return;
		}

		const ac = new AbortController();
		controller = ac;
		loading = true;
		loaded = 0;
		total = currentTotal;

		const snapshotTotal = currentTotal;

		(async () => {
			try {
				const randomTrackIndex = Math.floor(Math.random() * snapshotTotal);
				const initialOffset = randomTrackIndex - (randomTrackIndex % config.pageSize);

				const initialPage = await config.fetchPage(config.pageSize, initialOffset, ac.signal);
				if (ac.signal.aborted) return;

				const initialQueue = config.buildQueue(initialPage.items);
				if (initialQueue.length === 0) return;
				const requestedIndex = randomTrackIndex - initialOffset;
				const startIndex = config.resolveShuffleStartIndex
					? config.resolveShuffleStartIndex(initialPage.items, requestedIndex, initialQueue)
					: Math.min(requestedIndex, initialQueue.length - 1);
				onPlayQueue(initialQueue, Math.max(startIndex, 0), true);
				loaded = initialPage.items.length;

				const allOffsets: number[] = [];
				for (let o = 0; o < snapshotTotal; o += config.pageSize) {
					if (o !== initialOffset) allOffsets.push(o);
				}
				shuffleInPlace(allOffsets);

				for (const offset of allOffsets) {
					if (ac.signal.aborted) return;
					const batchPage = await config.fetchPage(config.pageSize, offset, ac.signal);
					if (ac.signal.aborted) return;

					const batchQueue = config.buildQueue(batchPage.items);
					if (batchQueue.length > 0) {
						onQueueAppend(batchQueue);
						onShuffleRegenerate();
					}
					loaded += batchPage.items.length;
				}
			} catch (_e: unknown) {
				if (ac.signal.aborted) return;
				onToast("Couldn't finish loading the shuffled tracks.", 'error');
			} finally {
				if (controller === ac) {
					loading = false;
					controller = null;
				}
			}
		})();
	}

	function getProgressText(): string | null {
		if (!loading) return null;
		return `Loading ${loaded.toLocaleString()} of ${total.toLocaleString()}`;
	}

	return {
		get loading() {
			return loading;
		},
		get loaded() {
			return loaded;
		},
		get total() {
			return total;
		},
		get progressText() {
			return getProgressText();
		},
		playAll,
		shuffleAll,
		abort,
		get controller() {
			return controller;
		}
	};
}
