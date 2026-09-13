import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invalidate = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));

vi.mock('./LibraryManagementInvalidation', () => ({
	invalidateLibraryManagementSurfaces: invalidate
}));

import {
	createLibraryManagementEvents,
	parseLibraryManagementActivityEvent
} from './LibraryManagementEvents';
import { createMuxEventStream, type MuxEventStream } from '$lib/queries/events/MuxEventStream';

class FakeEventSource {
	static instances: FakeEventSource[] = [];
	readonly url: string;
	readonly listeners = new Map<string, Set<EventListener>>();
	onopen: ((event: Event) => void) | null = null;
	closed = false;

	constructor(url: string | URL) {
		this.url = String(url);
		FakeEventSource.instances.push(this);
	}

	addEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
		const callback = listener as EventListener;
		const listeners = this.listeners.get(type) ?? new Set<EventListener>();
		listeners.add(callback);
		this.listeners.set(type, listeners);
	}

	removeEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
		this.listeners.get(type)?.delete(listener as EventListener);
	}

	close(): void {
		this.closed = true;
	}

	emit(type: string, data = '', lastEventId = ''): void {
		const event = type === 'open' ? new Event(type) : new MessageEvent(type, { data, lastEventId });
		for (const listener of this.listeners.get(type) ?? []) listener(event);
	}

	emitOpen(): void {
		this.onopen?.(new Event('open'));
	}
}

let mux: MuxEventStream;

beforeEach(() => {
	vi.clearAllMocks();
	FakeEventSource.instances = [];
	vi.stubGlobal('EventSource', FakeEventSource);
	mux = createMuxEventStream();
	mux.connect();
});

afterEach(() => {
	mux.disconnect();
});

describe('parseLibraryManagementActivityEvent', () => {
	it('accepts the durable revision payload and rejects malformed revisions', () => {
		expect(
			parseLibraryManagementActivityEvent(
				'{"id":"activity:4","revisions":{"operation":4,"scan":2}}'
			)
		).toEqual({ id: 'activity:4', revisions: { operation: 4, scan: 2 } });
		expect(parseLibraryManagementActivityEvent('{"id":"activity:4","revisions":[]}')).toBeNull();
		expect(
			parseLibraryManagementActivityEvent('{"id":"activity:4","revisions":{"operation":"four"}}')
		).toBeNull();
		expect(parseLibraryManagementActivityEvent('not-json')).toBeNull();
	});
});

describe('createLibraryManagementEvents', () => {
	it('refreshes on start and de-duplicates replayed event IDs', () => {
		const events = createLibraryManagementEvents(mux);
		events.start();
		const first = FakeEventSource.instances[0];
		expect(FakeEventSource.instances).toHaveLength(1);
		expect(first.url).toBe('/api/v1/events/stream');

		// mount parity: the retired page stream refreshed on open, so start()
		// refreshes directly.
		expect(invalidate).toHaveBeenCalledOnce();
		invalidate.mockClear();
		const payload = '{"id":"activity:7","revisions":{"operation":7}}';
		first.emit('activity.changed', payload, 'activity:7');
		first.emit('activity.changed', payload, 'activity:7');
		expect(invalidate).toHaveBeenCalledOnce();

		events.start();
		expect(invalidate).toHaveBeenCalledTimes(2);
		invalidate.mockClear();
		first.emit('activity.changed', payload, 'activity:7');
		expect(invalidate).not.toHaveBeenCalled();
		first.emit('activity.changed', '{"id":"activity:8","revisions":{"operation":8}}', 'activity:8');
		expect(invalidate).toHaveBeenCalledOnce();

		events.stop();
		first.emit('activity.changed', '{"id":"activity:9","revisions":{"operation":9}}', 'activity:9');
		expect(invalidate).toHaveBeenCalledOnce();
	});

	it('refreshes when the mux reconnects', () => {
		const events = createLibraryManagementEvents(mux);
		events.start();
		invalidate.mockClear();

		FakeEventSource.instances[0].emitOpen();
		expect(invalidate).toHaveBeenCalledOnce();
	});

	it('stays silent on reconnect after stop', () => {
		const events = createLibraryManagementEvents(mux);
		events.start();
		invalidate.mockClear();
		events.stop();
		FakeEventSource.instances[0].emitOpen();
		expect(invalidate).not.toHaveBeenCalled();
	});

	it('defers the start refresh to the first open when starting disconnected', () => {
		const idle = createMuxEventStream();
		const events = createLibraryManagementEvents(idle);
		events.start();
		expect(invalidate).not.toHaveBeenCalled();
		idle.connect();
		expect(FakeEventSource.instances).toHaveLength(2);
		FakeEventSource.instances[1].emitOpen();
		expect(invalidate).toHaveBeenCalledOnce();
		idle.disconnect();
	});

	it('invalidates distinct revision vectors even when their maximum is unchanged', () => {
		const events = createLibraryManagementEvents(mux);
		events.start();
		invalidate.mockClear();
		const source = FakeEventSource.instances[0];
		source.emit(
			'activity.changed',
			'{"id":"activity:first","revisions":{"scan":100,"operation":5}}',
			'activity:first'
		);
		source.emit(
			'activity.changed',
			'{"id":"activity:second","revisions":{"scan":100,"operation":6}}',
			'activity:second'
		);

		expect(invalidate).toHaveBeenCalledTimes(2);
	});

	it('ignores malformed stream payloads', () => {
		const events = createLibraryManagementEvents(mux);
		events.start();
		invalidate.mockClear();
		FakeEventSource.instances[0].emit('activity.changed', '{"id":"bad"}');
		expect(invalidate).not.toHaveBeenCalled();
	});
});
