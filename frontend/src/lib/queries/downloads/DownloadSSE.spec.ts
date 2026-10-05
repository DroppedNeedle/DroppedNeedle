import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { setDownloadScope } from './downloadScope.svelte';

const { invalidate } = vi.hoisted(() => ({ invalidate: vi.fn().mockResolvedValue(undefined) }));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: invalidate
}));

class FakeEventSource {
	static instances: FakeEventSource[] = [];
	url: string;
	listeners: Record<string, ((e: MessageEvent) => void)[]> = {};
	closed = false;

	constructor(url: string) {
		this.url = url;
		FakeEventSource.instances.push(this);
	}

	addEventListener(type: string, cb: (e: MessageEvent) => void) {
		(this.listeners[type] ??= []).push(cb);
	}

	close() {
		this.closed = true;
	}

	emit(type: string, data: unknown) {
		const ev = { data: JSON.stringify(data) } as MessageEvent;
		for (const cb of this.listeners[type] ?? []) cb(ev);
	}
}

beforeEach(() => {
	vi.useFakeTimers();
	setDownloadScope('user', 'user');
	invalidate.mockClear();
	FakeEventSource.instances = [];
	vi.stubGlobal('EventSource', FakeEventSource as unknown as typeof EventSource);
});

afterEach(() => {
	vi.runAllTimers();
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

const { createDownloadStream, getOrganizerRetry } = await import('./DownloadSSE.svelte');

describe('createDownloadStream', () => {
	it('coalesces structural bursts without a progress request storm', () => {
		const stream = createDownloadStream();
		stream.start('task');
		const events = FakeEventSource.instances[0];
		for (let i = 0; i < 100; i++) events.emit('progress', { bytes_downloaded: i });
		vi.advanceTimersByTime(100);
		expect(invalidate).not.toHaveBeenCalled();
		events.emit('status', { status: 'processing' });
		events.emit('complete', { status: 'completed' });
		vi.advanceTimersByTime(100);
		expect(invalidate).toHaveBeenCalledTimes(1);
	});

	it('fences events and scheduled invalidation across same-user role changes', () => {
		const stream = createDownloadStream();
		stream.start('task');
		const events = FakeEventSource.instances[0];
		events.emit('status', { status: 'processing' });
		setDownloadScope('user', 'admin');
		events.emit('complete', { status: 'completed' });
		vi.advanceTimersByTime(100);
		expect(stream.state.done).toBe(false);
		expect(invalidate).not.toHaveBeenCalled();
	});
});

describe('organizerRetry', () => {
	function retryStream(taskId: string): FakeEventSource {
		const s = createDownloadStream();
		s.start(taskId);
		return FakeEventSource.instances[FakeEventSource.instances.length - 1];
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
