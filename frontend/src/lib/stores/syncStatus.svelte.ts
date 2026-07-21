import { browser } from '$app/environment';
import { api } from '$lib/api/client';
import { getApiUrl } from '$lib/api/api-utils';

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

const MAX_RECONNECT_ATTEMPTS = 5;
const POLL_ACTIVE_MS = 1500;
const POLL_IDLE_MS = 5000;
const AUTO_HIDE_SUCCESS_MS = 4000;
const AUTO_HIDE_ERROR_MS = 6000;

function createSyncStatusStore() {
	let status = $state<SyncStatus>({ ...EMPTY_STATUS });
	let isDismissed = $state(false);
	let isMinimized = $state(false);
	let showIndicator = $state(false);
	let connectionMode = $state<'sse' | 'polling'>('sse');

	let eventSource: EventSource | null = null;
	let pollInterval: ReturnType<typeof setInterval> | null = null;
	let hideTimeout: ReturnType<typeof setTimeout> | null = null;
	let reconnectTimeout: ReturnType<typeof setTimeout> | null = null;
	let reconnectAttempts = 0;
	let connected = false;

	function clearAllTimers(): void {
		if (pollInterval) {
			clearInterval(pollInterval);
			pollInterval = null;
		}
		if (hideTimeout) {
			clearTimeout(hideTimeout);
			hideTimeout = null;
		}
		if (reconnectTimeout) {
			clearTimeout(reconnectTimeout);
			reconnectTimeout = null;
		}
	}

	function applyStatus(newStatus: SyncStatus): void {
		const wasSyncing = status.is_syncing;
		status = newStatus;

		if (newStatus.is_syncing && !wasSyncing) {
			isDismissed = false;
			isMinimized = false;
		}

		handleStatusUpdate(newStatus);

		if (connectionMode === 'polling' && wasSyncing !== newStatus.is_syncing) {
			schedulePoll();
		}
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

	function connectSSE(): void {
		if (!browser || document.hidden) return;
		if (eventSource) {
			eventSource.close();
			eventSource = null;
		}

		eventSource = new EventSource(getApiUrl('/api/v1/cache/sync/stream'));

		eventSource.onopen = () => {
			connectionMode = 'sse';
			reconnectAttempts = 0;
			if (pollInterval) {
				clearInterval(pollInterval);
				pollInterval = null;
			}
		};

		eventSource.onmessage = (event) => {
			try {
				applyStatus(JSON.parse(event.data));
			} catch {
				// ignore malformed messages
			}
		};

		eventSource.onerror = () => {
			eventSource?.close();
			eventSource = null;
			reconnectAttempts++;

			if (reconnectTimeout) {
				clearTimeout(reconnectTimeout);
				reconnectTimeout = null;
			}

			if (status.is_syncing && !pollInterval) {
				startPolling();
			}

			if (reconnectAttempts < MAX_RECONNECT_ATTEMPTS) {
				const delay = Math.min(1000 * Math.pow(2, reconnectAttempts), 30000);
				reconnectTimeout = setTimeout(() => {
					reconnectTimeout = null;
					if (connected && !document.hidden) connectSSE();
				}, delay);
			} else {
				connectionMode = 'polling';
				if (!pollInterval) startPolling();
			}
		};
	}

	async function fetchStatus(): Promise<void> {
		try {
			const data = await api.global.get<SyncStatus>('/api/v1/cache/sync/status');
			applyStatus(data);
		} catch {
			// ignore fetch errors
		}
	}

	function startPolling(): void {
		if (pollInterval) {
			clearInterval(pollInterval);
			pollInterval = null;
		}
		void fetchStatus();
		schedulePoll();
	}

	function schedulePoll(): void {
		if (pollInterval) {
			clearInterval(pollInterval);
		}
		pollInterval = setInterval(
			() => void fetchStatus(),
			status.is_syncing ? POLL_ACTIVE_MS : POLL_IDLE_MS
		);
	}

	function handleVisibilityChange(): void {
		if (!connected) return;
		if (document.hidden) {
			if (reconnectTimeout) {
				clearTimeout(reconnectTimeout);
				reconnectTimeout = null;
			}
			eventSource?.close();
			eventSource = null;
			if (pollInterval) {
				clearInterval(pollInterval);
				pollInterval = null;
			}
		} else {
			if (connectionMode === 'sse') {
				reconnectAttempts = 0;
				connectSSE();
			} else {
				startPolling();
			}
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
		get connectionMode() {
			return connectionMode;
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
			connectSSE();
			document.addEventListener('visibilitychange', handleVisibilityChange);
		},

		disconnect(): void {
			if (!browser) return;
			connected = false;
			eventSource?.close();
			eventSource = null;
			clearAllTimers();
			document.removeEventListener('visibilitychange', handleVisibilityChange);
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
				await api.global.post('/api/v1/cache/sync/cancel');
			} catch {
				// ignore errors, sync may already be stopped
			}
		}
	};
}

export const syncStatus = createSyncStatusStore();
