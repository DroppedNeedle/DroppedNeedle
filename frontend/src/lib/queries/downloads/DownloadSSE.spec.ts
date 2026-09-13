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

const { createDownloadStream, getOrganizerRetry, resetOrganizerRetry } =
	await import('./DownloadSSE.svelte');

describe('createDownloadStream', () => {
	it('maps progress events to rune state', () => {
		const s = createDownloadStream();
		s.start('t1');
		FakeEventSource.instances[0].emit('progress', {
			bytes_downloaded: 5,
			bytes_total: 10,
			files_completed: 1,
			files_total: 2,
			progress_percent: 50,
			candidate_index: 1,
			source: 'soulseek',
			quality_format: 'flac',
			quality_bit_depth: 16,
			quality_sample_rate: 44100,
			advertised_queue_depth: 0,
			queue_position_start: 91,
			queue_position_end: 100,
			remote_queued: true,
			preferred_quality_fallback_at: 1234.5,
			attempt_number: 1,
			attempt_total: 3,
			has_next_source: true
		});
		expect(s.state.progress?.progress_percent).toBe(50);
		expect(s.state.progress?.bytes_total).toBe(10);
		expect(s.state.source).toEqual({
			candidate_index: 1,
			source: 'soulseek',
			quality_format: 'flac',
			quality_bit_depth: 16,
			quality_sample_rate: 44100,
			advertised_queue_depth: 0,
			queue_position_start: 91,
			queue_position_end: 100,
			remote_queued: true,
			preferred_quality_fallback_at: 1234.5,
			attempt_number: 1,
			attempt_total: 3,
			has_next_source: true
		});
	});

	it('captures status events', () => {
		const s = createDownloadStream();
		s.start('t1');
		FakeEventSource.instances[0].emit('progress', {
			queue_position_start: 91,
			queue_position_end: 100
		});
		FakeEventSource.instances[0].emit('status', {
			status: 'retrying',
			candidate_index: 1,
			source: 'soulseek',
			quality_format: 'flac',
			quality_bit_depth: 16,
			quality_sample_rate: 44100,
			advertised_queue_depth: 0,
			queue_position_start: null,
			queue_position_end: null,
			remote_queued: false,
			attempt: 2,
			attempt_total: 3,
			has_next_source: true
		});
		expect(s.state.status).toBe('retrying');
		expect(s.state.source?.attempt_number).toBe(2);
		expect(s.state.source?.quality_bit_depth).toBe(16);
		expect(s.state.source?.candidate_index).toBe(1);
		expect(s.state.source?.queue_position_start).toBeNull();
		expect(s.state.source?.remote_queued).toBe(false);
	});

	it('marks done and closes the stream on the complete event', () => {
		const s = createDownloadStream();
		s.start('t1');
		const es = FakeEventSource.instances[0];
		es.emit('complete', { status: 'completed' });
		expect(s.state.done).toBe(true);
		expect(s.state.status).toBe('completed');
		expect(es.closed).toBe(true);
	});

	it('stop() closes the underlying EventSource', () => {
		const s = createDownloadStream();
		s.start('t1');
		const es = FakeEventSource.instances[0];
		s.stop();
		expect(es.closed).toBe(true);
	});

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

	it('resets the entry on invalidation so a refresh re-attaches from live state', () => {
		const events = retryStream('retry-reset');
		events.emit('organizer_retry', {
			state: 'running',
			stage: 'publishing',
			files_completed: 7,
			files_total: 13,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		expect(getOrganizerRetry('retry-reset')).not.toBeNull();
		resetOrganizerRetry('retry-reset');
		expect(getOrganizerRetry('retry-reset')).toBeNull();
	});

	it('accepts a fresh retry after the previous terminal settled and reset', () => {
		const events = retryStream('retry-again');
		events.emit('organizer_retry', {
			state: 'complete',
			stage: 'finalizing',
			files_completed: 13,
			files_total: 13,
			files_imported: 13,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		expect(getOrganizerRetry('retry-again')?.state).toBe('complete');
		resetOrganizerRetry('retry-again');
		events.emit('organizer_retry', {
			state: 'running',
			stage: 'preparing',
			files_completed: 0,
			files_total: 13,
			updated_at: '2026-09-13T22:00:01+00:00'
		});
		expect(getOrganizerRetry('retry-again')).toMatchObject({
			state: 'running',
			stage: 'preparing'
		});
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

	it('carries the failure message on a failed terminal', () => {
		const events = retryStream('retry-failed');
		events.emit('organizer_retry', {
			state: 'failed',
			stage: 'publishing',
			files_completed: 7,
			files_total: 13,
			error: 'The library destination was unavailable.',
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		expect(getOrganizerRetry('retry-failed')).toMatchObject({
			state: 'failed',
			error: 'The library destination was unavailable.'
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
