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

const { createDownloadStream, getOrganizerRetry } = await import('./DownloadSSE.svelte');

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

describe('organizerRetry', () => {
	function retryStream(taskId: string) {
		const { mux, emit } = fakeMux();
		createDownloadStream(mux).start(taskId);
		return {
			emit: (name: string, data: Record<string, unknown>) =>
				emit(name, { ...data, task_id: taskId })
		};
	}

	it('keys snapshots by task id with latest-wins ordering', () => {
		const a = retryStream('retry-a');
		const b = retryStream('retry-b');
		a.emit('organizer_retry', {
			state: 'running',
			stage: 'preparing',
			files_completed: 0,
			files_total: 13,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		a.emit('organizer_retry', {
			state: 'running',
			stage: 'publishing',
			files_completed: 7,
			files_total: 13,
			updated_at: '2026-09-13T22:00:01+00:00'
		});
		b.emit('organizer_retry', {
			state: 'running',
			stage: 'planning',
			files_completed: 0,
			files_total: 4,
			updated_at: '2026-09-13T22:00:02+00:00'
		});
		expect(getOrganizerRetry('retry-a')).toMatchObject({
			state: 'running',
			stage: 'publishing',
			files_completed: 7,
			files_total: 13
		});
		expect(getOrganizerRetry('retry-b')).toMatchObject({
			state: 'running',
			stage: 'planning',
			files_completed: 0,
			files_total: 4
		});
		expect(getOrganizerRetry('retry-unknown')).toBeNull();
	});

	it('settles terminal states exactly once and rejects a complete-then-failed double settle', () => {
		const events = retryStream('retry-settle');
		events.emit('organizer_retry', {
			state: 'complete',
			stage: 'finalizing',
			files_completed: 13,
			files_total: 13,
			files_imported: 13,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		events.emit('organizer_retry', {
			state: 'failed',
			stage: 'publishing',
			files_completed: 7,
			files_total: 13,
			error: 'Contradictory late failure',
			updated_at: '2026-09-13T22:00:01+00:00'
		});
		events.emit('organizer_retry', {
			state: 'running',
			stage: 'preparing',
			files_completed: 0,
			files_total: 13,
			updated_at: '2026-09-13T22:00:02+00:00'
		});
		expect(getOrganizerRetry('retry-settle')).toMatchObject({
			state: 'complete',
			stage: 'finalizing',
			files_imported: 13,
			error: null
		});
	});

	it('ignores malformed organizer_retry payloads without clobbering state', () => {
		const events = retryStream('retry-malformed');
		events.emit('organizer_retry', { state: 'running', stage: 'publishing' });
		expect(getOrganizerRetry('retry-malformed')).toMatchObject({ stage: 'publishing' });
		events.emit('organizer_retry', { state: 'exploding', stage: 'publishing' });
		events.emit('organizer_retry', { state: 'failed', stage: 'rebobulating' });
		events.emit('organizer_retry', { nope: true });
		expect(getOrganizerRetry('retry-malformed')).toMatchObject({
			state: 'running',
			stage: 'publishing'
		});
	});
});
