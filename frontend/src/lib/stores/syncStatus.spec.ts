import { beforeEach, describe, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }));

vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$lib/api/client', () => ({
	api: { global: { get: state.get, post: state.post } }
}));

import { API } from '$lib/constants';
import type {
	MuxEventListener,
	MuxEventStream,
	MuxUnsubscribe
} from '$lib/queries/events/MuxEventStream';
import { createSyncStatusStore } from './syncStatus.svelte';

const IDLE = {
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

const SYNCING = { ...IDLE, is_syncing: true, phase: 'artists', progress_percent: 42 };

function fakeMux() {
	const listeners = new Map<string, Set<MuxEventListener>>();
	const mux: MuxEventStream = {
		connect: () => {},
		disconnect: () => {},
		on(eventName: string, listener: MuxEventListener): MuxUnsubscribe {
			let set = listeners.get(eventName);
			if (!set) {
				set = new Set();
				listeners.set(eventName, set);
			}
			set.add(listener);
			return () => {
				listeners.get(eventName)?.delete(listener);
			};
		},
		onConnect: () => () => {},
		isConnected: true
	};
	return {
		mux,
		emit(eventName: string, data: unknown): void {
			const event = new MessageEvent(eventName, {
				data: typeof data === 'string' ? data : JSON.stringify(data)
			});
			for (const listener of listeners.get(eventName) ?? []) listener(event);
		}
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	state.get.mockResolvedValue({ ...IDLE });
	state.post.mockResolvedValue(undefined);
});

describe('syncStatus', () => {
	it('seeds current status from /status on connect', async () => {
		state.get.mockResolvedValue({ ...SYNCING });
		const { mux } = fakeMux();
		const store = createSyncStatusStore(mux);

		store.connect();
		await vi.waitFor(() => expect(store.isActive).toBe(true));

		expect(state.get).toHaveBeenCalledWith(API.cacheSync.status());
		expect(store.phase).toBe('artists');
		expect(store.progress).toBe(42);
		store.disconnect();
	});

	it('applies live cache.sync frames', async () => {
		const { mux, emit } = fakeMux();
		const store = createSyncStatusStore(mux);

		store.connect();
		await vi.waitFor(() => expect(state.get).toHaveBeenCalled());
		emit('cache.sync', { ...SYNCING });

		expect(store.isActive).toBe(true);
		expect(store.showIndicator).toBe(true);
		store.disconnect();
	});

	it('prefers a live frame over a slower seed', async () => {
		let resolveFetch!: (value: unknown) => void;
		state.get.mockReturnValue(
			new Promise((resolve) => {
				resolveFetch = resolve;
			})
		);
		const { mux, emit } = fakeMux();
		const store = createSyncStatusStore(mux);

		store.connect();
		emit('cache.sync', { ...SYNCING });
		resolveFetch({ ...IDLE });
		await new Promise((resolve) => setTimeout(resolve, 0));

		expect(store.isActive).toBe(true);
		store.disconnect();
	});

	it('goes silent after disconnect', async () => {
		const { mux, emit } = fakeMux();
		const store = createSyncStatusStore(mux);

		store.connect();
		await vi.waitFor(() => expect(state.get).toHaveBeenCalled());
		store.disconnect();
		emit('cache.sync', { ...SYNCING });

		expect(store.isActive).toBe(false);
	});

	it('ignores malformed cache.sync frames', async () => {
		const { mux, emit } = fakeMux();
		const store = createSyncStatusStore(mux);

		store.connect();
		await vi.waitFor(() => expect(state.get).toHaveBeenCalled());
		emit('cache.sync', 'not-json{{{');

		expect(store.isActive).toBe(false);
		store.disconnect();
	});

	it('cancels the sync over HTTP', async () => {
		const { mux } = fakeMux();
		const store = createSyncStatusStore(mux);

		await store.cancelSync();

		expect(state.post).toHaveBeenCalledWith(API.cacheSync.cancel());
	});
});
