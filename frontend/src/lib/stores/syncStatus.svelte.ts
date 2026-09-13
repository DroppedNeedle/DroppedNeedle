import { browser } from '$app/environment';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import {
	muxEventStream,
	type MuxEventStream,
	type MuxUnsubscribe
} from '$lib/queries/events/MuxEventStream';

type SyncStatus = {
	is_syncing: boolean;
	phase: string | null;
	total_items: number;
	processed_items: number;
	progress_percent: number;
	current_item: string | null;
	error_message: string | null;
	total_artists: number;
	processed_artists: number;
	total_albums: number;
	processed_albums: number;
};

const PHASE_LABELS: Record<string, string> = {
	artists: 'Artist Images',
	discovery: 'Artist Discovery',
	albums: 'Album Data',
	audiodb_prewarm: 'AudioDB Images'
};

const PHASE_ORDER = ['artists', 'discovery', 'albums', 'audiodb_prewarm'];

const EMPTY_STATUS: SyncStatus = {
	is_syncing: false,
	phase: null,
	total_items: 0,
	processed_items: 0,
	progress_percent: 0,
	current_item: null,
	error_message: null,
	total_artists: 0,
	processed_artists: 0,
	total_albums: 0,
	processed_albums: 0
};

const AUTO_HIDE_SUCCESS_MS = 4000;
const AUTO_HIDE_ERROR_MS = 6000;

export function createSyncStatusStore(mux: MuxEventStream = muxEventStream) {
	let status = $state<SyncStatus>({ ...EMPTY_STATUS });
	let isDismissed = $state(false);
	let isMinimized = $state(false);
	let showIndicator = $state(false);

	let unsubCacheSync: MuxUnsubscribe | null = null;
	let hideTimeout: ReturnType<typeof setTimeout> | null = null;
	let liveGeneration = 0;
	let connected = false;

	function applyStatus(newStatus: SyncStatus): void {
		const wasSyncing = status.is_syncing;
		status = newStatus;

		if (newStatus.is_syncing && !wasSyncing) {
			isDismissed = false;
			isMinimized = false;
		}

		handleStatusUpdate(newStatus);
	}

	function handleStatusUpdate(newStatus: SyncStatus): void {
		if (newStatus.is_syncing) {
			if (hideTimeout) {
				clearTimeout(hideTimeout);
				hideTimeout = null;
			}
			if (!isDismissed) {
				showIndicator = true;
			}
		} else if (newStatus.error_message) {
			showIndicator = true;
			if (!hideTimeout) {
				hideTimeout = setTimeout(() => {
					showIndicator = false;
					hideTimeout = null;
				}, AUTO_HIDE_ERROR_MS);
			}
		} else if (showIndicator && !hideTimeout) {
			hideTimeout = setTimeout(() => {
				showIndicator = false;
				hideTimeout = null;
			}, AUTO_HIDE_SUCCESS_MS);
		}
	}

	function onCacheSync(event: Event): void {
		if (!(event instanceof MessageEvent) || typeof event.data !== 'string') return;
		try {
			applyStatus(JSON.parse(event.data) as SyncStatus);
		} catch {
			// ignore malformed messages without bumping the generation, so a
			// bad frame never kills an in-flight seed without applying anything
			return;
		}
		liveGeneration += 1;
	}

	async function fetchStatus(): Promise<void> {
		const basis = liveGeneration;
		try {
			const data = await api.global.get<SyncStatus>(API.cacheSync.status());
			// A live frame that landed mid-fetch is fresher than this seed.
			if (basis !== liveGeneration) return;
			applyStatus(data);
		} catch {
			// ignore fetch errors
		}
	}

	return {
		get status() {
			return status;
		},
		get isActive() {
			return status.is_syncing;
		},
		get phase() {
			return status.phase;
		},
		get progress() {
			return status.progress_percent;
		},
		get currentItem() {
			return status.current_item;
		},
		get error() {
			return status.error_message;
		},
		get totalItems() {
			return status.total_items;
		},
		get processedItems() {
			return status.processed_items;
		},
		get isDismissed() {
			return isDismissed;
		},
		get showIndicator() {
			return showIndicator && !isDismissed;
		},
		get isMinimized() {
			return isMinimized;
		},
		get phaseLabel() {
			return status.phase ? (PHASE_LABELS[status.phase] ?? 'Syncing') : 'Library';
		},
		get phaseNumber() {
			if (!status.phase) return 0;
			const idx = PHASE_ORDER.indexOf(status.phase);
			return idx >= 0 ? idx + 1 : 0;
		},
		get totalPhases() {
			return PHASE_ORDER.length;
		},

		connect(): void {
			if (!browser || connected) return;
			connected = true;
			unsubCacheSync = mux.on('cache.sync', onCacheSync);
			// The mux yields its initial snapshot once per connection, which
			// may predate this registration (connect runs deferred), so seed
			// directly. Drops reconnect natively at the server's retry frame;
			// no polling fallback: a fetch here plus live frames covers it.
			void fetchStatus();
		},

		disconnect(): void {
			if (!browser) return;
			connected = false;
			unsubCacheSync?.();
			unsubCacheSync = null;
			if (hideTimeout) {
				clearTimeout(hideTimeout);
				hideTimeout = null;
			}
		},

		dismiss(): void {
			isDismissed = true;
		},

		undismiss(): void {
			isDismissed = false;
		},

		minimize(): void {
			isMinimized = true;
		},

		expand(): void {
			isMinimized = false;
		},

		checkStatus(): void {
			void fetchStatus();
		},

		async cancelSync(): Promise<void> {
			try {
				await api.global.post(API.cacheSync.cancel());
			} catch {
				// ignore errors, sync may already be stopped
			}
		}
	};
}

export const syncStatus = createSyncStatusStore();
