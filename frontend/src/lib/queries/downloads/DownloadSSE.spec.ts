import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { MuxEventListener, MuxEventStream } from '$lib/queries/events/MuxEventStream';
import { setDownloadScope } from './downloadScope.svelte';

const { invalidate } = vi.hoisted(() => ({ invalidate: vi.fn().mockResolvedValue(undefined) }));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: invalidate
}));

// Stands in for the tab's shared event stream.
function fakeMux() {
	const listeners = new Map<string, Set<MuxEventListener>>();
	const mux: MuxEventStream = {
		connect: () => undefined,
		disconnect: () => undefined,
		on(name, listener) {
			let set = listeners.get(name);
			if (!set) listeners.set(name, (set = new Set()));
			set.add(listener);
			return () => set.delete(listener);
		},
		onConnect: () => () => undefined,
		isConnected: true
	};
	function emit(name: string, data: unknown) {
		const ev = { data: JSON.stringify(data) } as MessageEvent;
		for (const listener of listeners.get(name) ?? []) listener(ev);
	}
	return { mux, emit };
}

beforeEach(() => {
	vi.useFakeTimers();
	setDownloadScope('user', 'user');
	invalidate.mockClear();
});

afterEach(() => {
	vi.runAllTimers();
	vi.useRealTimers();
});

const { createDownloadStream } = await import('./DownloadSSE.svelte');

describe('createDownloadStream', () => {
	it('applies its own task progress and coalesces source changes', () => {
		const { mux, emit } = fakeMux();
		const stream = createDownloadStream(mux);
		stream.start('task');
		emit('download_progress', { task_id: 'task', status: 'downloading', bytes_downloaded: 1 });
		for (let i = 2; i < 100; i++) {
			emit('download_progress', { task_id: 'task', status: 'downloading', bytes_downloaded: i });
		}
		emit('download_progress', { task_id: 'other', status: 'processing', bytes_downloaded: 7 });
		vi.advanceTimersByTime(100);
		expect(invalidate).toHaveBeenCalledTimes(1);
		expect(stream.state.progress?.bytes_downloaded).toBe(99);
		stream.stop();
		emit('download_progress', { task_id: 'task', status: 'downloading', bytes_downloaded: 500 });
		expect(stream.state.progress?.bytes_downloaded).toBe(99);
	});

	it('fences events across same-user role changes', () => {
		const { mux, emit } = fakeMux();
		const stream = createDownloadStream(mux);
		stream.start('task');
		setDownloadScope('user', 'admin');
		emit('download_progress', { task_id: 'task', status: 'processing', bytes_downloaded: 5 });
		vi.advanceTimersByTime(100);
		expect(stream.state.progress).toBeNull();
		expect(invalidate).not.toHaveBeenCalled();
	});
});
