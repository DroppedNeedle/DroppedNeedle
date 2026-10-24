import { writable } from 'svelte/store';
import { browser } from '$app/environment';
import { API } from '$lib/constants';
import { getCacheTTLs } from '$lib/stores/cacheTtl.svelte';
import { api, ApiError } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { musicBrainzSourceKey } from '$lib/queries/musicbrainz/sourceScope.svelte';

export type QueueBuildStatus = 'idle' | 'building' | 'ready' | 'error' | 'unknown';

interface DiscoverQueueStatusState {
	status: QueueBuildStatus;
	queueId?: string;
	itemCount?: number;
	error?: string;
	lastChecked: number;
}

type QueueStatusPayload = {
	status: QueueBuildStatus;
	queue_id?: string;
	item_count?: number;
	error?: string;
};

const INITIAL: DiscoverQueueStatusState = {
	status: 'unknown',
	lastChecked: 0
};

function createDiscoverQueueStatusStore() {
	const { subscribe, set, update } = writable<DiscoverQueueStatusState>({ ...INITIAL });

	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let isPolling = false;
	let epoch = 0;
	let pendingGeneration: string | null = null;
	let latestStatusRequest = 0;
	let pollOwner = 0;
	const requestScope = () =>
		`${epoch}:${authStore.user?.id}:${JSON.stringify(musicBrainzSourceKey())}`;

	function getPollingInterval(): number {
		return getCacheTTLs().discoverQueuePollingInterval;
	}

	function applyStatusData(data: QueueStatusPayload): void {
		set({
			status: data.status,
			queueId: data.queue_id,
			itemCount: data.item_count,
			error: data.error,
			lastChecked: Date.now()
		});
	}

	async function fetchStatus(): Promise<QueueStatusPayload | null> {
		if (!browser) return null;
		const scope = requestScope();
		// A generation response owns the state until it settles. In particular,
		// polling must not publish an idle response from before the new build.
		if (pendingGeneration === scope) return null;
		const sequence = ++latestStatusRequest;
		try {
			const data = await api.global.get<QueueStatusPayload>(API.discoverQueueStatus());
			if (scope !== requestScope() || sequence !== latestStatusRequest) return null;
			applyStatusData(data);
			return scope === requestScope() && sequence === latestStatusRequest ? data : null;
		} catch {
			return null;
		}
	}

	async function triggerGenerate(force = false): Promise<void> {
		if (!browser) return;
		epoch++;
		stopPolling();
		const scope = requestScope();
		pendingGeneration = scope;
		try {
			update((s) => ({ ...s, status: 'building' }));
			const data = await api.global.post<QueueStatusPayload>(API.discoverQueueGenerate(), {
				force
			});
			if (scope !== requestScope()) return;
			pendingGeneration = null;
			applyStatusData(data);
			if (scope === requestScope() && data.status === 'building') {
				startPolling();
			}
		} catch (e) {
			if (scope !== requestScope()) return;
			pendingGeneration = null;
			if (e instanceof ApiError) {
				update((s) => ({
					...s,
					status: 'error',
					error: `Server responded with ${e.status}`
				}));
			} else {
				update((s) => ({ ...s, status: 'error', error: 'Failed to trigger generation' }));
			}
		} finally {
			if (pendingGeneration === scope) pendingGeneration = null;
		}
	}

	function startPolling(): void {
		if (pollTimer || !browser || pendingGeneration === requestScope()) return;
		isPolling = true;
		const scope = requestScope();
		const owner = ++pollOwner;
		let inFlight = false;
		const interval = getPollingInterval();
		pollTimer = setInterval(async () => {
			if (owner !== pollOwner) return;
			if (scope !== requestScope()) {
				stopPolling();
				return;
			}
			if (inFlight) return;
			inFlight = true;
			try {
				const result = await fetchStatus();
				if (owner !== pollOwner || scope !== requestScope()) return;
				if (result && result.status !== 'building') stopPolling();
			} finally {
				inFlight = false;
			}
		}, interval);
	}

	function stopPolling(): void {
		pollOwner++;
		if (pollTimer) {
			clearInterval(pollTimer);
			pollTimer = null;
		}
		isPolling = false;
	}

	async function init(): Promise<void> {
		if (!browser) return;
		const result = await fetchStatus();
		if (!result) return;

		if (result.status === 'building') {
			startPolling();
		}
	}

	function reset(): void {
		epoch++;
		pendingGeneration = null;
		stopPolling();
		set({ ...INITIAL });
	}

	function markConsumed(): void {
		update((s) => ({ ...s, status: 'idle', queueId: undefined, itemCount: undefined }));
	}

	return {
		subscribe,
		fetchStatus,
		triggerGenerate,
		startPolling,
		stopPolling,
		init,
		reset,
		markConsumed,
		get isPolling() {
			return isPolling;
		}
	};
}

export const discoverQueueStatusStore = createDiscoverQueueStatusStore();
