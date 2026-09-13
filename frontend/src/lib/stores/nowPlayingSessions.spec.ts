import { beforeEach, describe, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({ get: vi.fn() }));

vi.mock('$lib/api/client', () => ({
	api: { global: { get: state.get } }
}));

import type {
	MuxEventListener,
	MuxEventStream,
	MuxUnsubscribe
} from '$lib/queries/events/MuxEventStream';
import type { NowPlayingSession } from '$lib/types';
import { createNowPlayingStore } from './nowPlayingSessions.svelte';

const SESSION: NowPlayingSession = {
	id: 's1',
	user_name: 'userA',
	track_name: 'Track',
	artist_name: 'Artist',
	album_name: 'Album',
	cover_url: '',
	device_name: 'Web',
	is_paused: false
};

function fakeMux() {
	const listeners = new Map<string, Set<MuxEventListener>>();
	const on = vi.fn((eventName: string, listener: MuxEventListener): MuxUnsubscribe => {
		let set = listeners.get(eventName);
		if (!set) {
			set = new Set();
			listeners.set(eventName, set);
		}
		set.add(listener);
		return () => {
			listeners.get(eventName)?.delete(listener);
		};
	});
	const mux: MuxEventStream = {
		connect: () => {},
		disconnect: () => {},
		on,
		onConnect: () => () => {},
		isConnected: true
	};
	return {
		mux,
		on,
		emit(eventName: string, data: unknown): void {
			const event = new MessageEvent(eventName, { data: JSON.stringify(data) });
			for (const listener of listeners.get(eventName) ?? []) listener(event);
		}
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	state.get.mockResolvedValue({ sessions: [] });
});

describe('nowPlayingSessions', () => {
	it('hydrates over HTTP and subscribes to snapshots on start', async () => {
		state.get.mockResolvedValue({ sessions: [{ ...SESSION }] });
		const { mux, on } = fakeMux();
		const store = createNowPlayingStore(mux);

		store.start();
		await vi.waitFor(() => expect(store.sessions).toHaveLength(1));

		expect(state.get).toHaveBeenCalledWith('/api/v1/now-playing');
		expect(on).toHaveBeenCalledWith('snapshot', expect.any(Function));
		expect(store.sessions[0].id).toBe('s1');
		store.stop();
	});

	it('applies live snapshot frames', async () => {
		const { mux, emit } = fakeMux();
		const store = createNowPlayingStore(mux);

		store.start();
		await vi.waitFor(() => expect(state.get).toHaveBeenCalled());
		emit('snapshot', { sessions: [{ ...SESSION }] });

		expect(store.sessions).toHaveLength(1);
		expect(store.activeSessions).toHaveLength(1);
		store.stop();
	});

	it('ignores a second start while running', async () => {
		const { mux, on } = fakeMux();
		const store = createNowPlayingStore(mux);

		store.start();
		store.start();
		await vi.waitFor(() => expect(state.get).toHaveBeenCalled());

		expect(on).toHaveBeenCalledTimes(1);
		store.stop();
	});

	it('unsubscribes and clears sessions on stop', async () => {
		const { mux, emit } = fakeMux();
		const store = createNowPlayingStore(mux);

		store.start();
		await vi.waitFor(() => expect(state.get).toHaveBeenCalled());
		store.stop();
		emit('snapshot', { sessions: [{ ...SESSION }] });

		expect(store.sessions).toHaveLength(0);
	});
});
