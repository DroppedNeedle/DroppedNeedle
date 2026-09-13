import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	invalidate: vi.fn().mockResolvedValue(undefined),
	invalidateCatalog: vi.fn().mockResolvedValue(undefined),
	activityData: undefined as { revisions: Record<string, number> } | undefined,
	queryCacheListener: undefined as (() => void) | undefined
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: h.invalidate,
	queryClient: {
		getQueryData: () => h.activityData,
		getQueryCache: () => ({
			subscribe: (listener: () => void) => {
				h.queryCacheListener = listener;
				return () => {
					h.queryCacheListener = undefined;
				};
			}
		})
	}
}));
vi.mock('./LibraryCatalogInvalidation', () => ({
	invalidateLibraryCatalog: h.invalidateCatalog
}));

import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { createMuxEventStream, type MuxEventStream } from '$lib/queries/events/MuxEventStream';
import { createLibraryActivityEvents } from './LibraryActivityEvents';

class FakeEventSource {
	static instances: FakeEventSource[] = [];
	readonly url: string;
	readonly listeners = new Map<string, Set<(event: Event) => void>>();
	onopen: ((event: Event) => void) | null = null;
	closed = false;

	constructor(url: string | URL) {
		this.url = String(url);
		FakeEventSource.instances.push(this);
	}

	addEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
		const callback = listener as (event: Event) => void;
		const listeners = this.listeners.get(type) ?? new Set<(event: Event) => void>();
		listeners.add(callback);
		this.listeners.set(type, listeners);
	}

	removeEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
		this.listeners.get(type)?.delete(listener as (event: Event) => void);
	}

	close(): void {
		this.closed = true;
	}

	emit(type: string, event: Event = new Event(type)): void {
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
	h.activityData = undefined;
	h.queryCacheListener = undefined;
	vi.stubGlobal('EventSource', FakeEventSource);
	mux = createMuxEventStream();
	mux.connect();
});

afterEach(() => {
	mux.disconnect();
});

describe('createLibraryActivityEvents', () => {
	it('opens no stream of its own and seeds the same initial revision silently', () => {
		const events = createLibraryActivityEvents(mux);
		events.start(true, 'user-1');
		expect(FakeEventSource.instances).toHaveLength(1);
		expect(FakeEventSource.instances[0].url).toBe('/api/v1/events/stream');

		// mount parity: the retired operations stream invalidated admin
		// surfaces on open, so start() does it directly.
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.operationsPrefix()
		});
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.reviewsPrefix()
		});
		expect(h.invalidate).not.toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.activityPrefix()
		});

		h.invalidate.mockClear();
		const initial = revisionEvent({ scan: 1, identification: 2, operation: 3, catalog: 4 });
		FakeEventSource.instances[0].emit('activity.changed', initial);
		expect(h.invalidate).not.toHaveBeenCalled();
		expect(h.invalidateCatalog).not.toHaveBeenCalled();
	});

	it('invalidates activity and admin surfaces once per genuine change', () => {
		const events = createLibraryActivityEvents(mux);
		events.start(true, 'user-1');
		const initial = revisionEvent({ scan: 1, identification: 2, operation: 3, catalog: 4 });
		FakeEventSource.instances[0].emit('activity.changed', initial);
		h.invalidate.mockClear();

		const changed = revisionEvent({ scan: 1, identification: 2, operation: 4, catalog: 4 });
		FakeEventSource.instances[0].emit('activity.changed', changed);

		expect(h.invalidate).toHaveBeenCalledTimes(3);
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.activityPrefix()
		});
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.operationsPrefix()
		});
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.reviewsPrefix()
		});
		expect(h.invalidateCatalog).not.toHaveBeenCalled();
	});

	it('uses one catalog sweep for a catalog revision change', () => {
		const events = createLibraryActivityEvents(mux);
		events.start(true, 'user-1');
		FakeEventSource.instances[0].emit(
			'activity.changed',
			revisionEvent({ scan: 1, identification: 2, operation: 3, catalog: 4 })
		);
		h.invalidate.mockClear();

		const changed = revisionEvent({ scan: 1, identification: 2, operation: 3, catalog: 5 });
		FakeEventSource.instances[0].emit('activity.changed', changed);

		expect(h.invalidateCatalog).toHaveBeenCalledOnce();
		expect(h.invalidate).not.toHaveBeenCalled();
	});

	it('refreshes admin surfaces on reconnect but ignores the replayed revision', () => {
		const events = createLibraryActivityEvents(mux);
		events.start(true, 'user-1');
		const initial = revisionEvent({ scan: 1, identification: 2, operation: 3, catalog: 4 });
		FakeEventSource.instances[0].emit('activity.changed', initial);

		h.invalidate.mockClear();
		FakeEventSource.instances[0].emitOpen();
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.operationsPrefix()
		});
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.reviewsPrefix()
		});
		expect(h.invalidate).not.toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.activityPrefix()
		});

		h.invalidate.mockClear();
		FakeEventSource.instances[0].emit('activity.changed', initial);
		expect(h.invalidate).not.toHaveBeenCalled();

		FakeEventSource.instances[0].emit(
			'activity.changed',
			revisionEvent({ scan: 1, identification: 3, operation: 3, catalog: 4 })
		);
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.activityPrefix()
		});
	});

	it('limits non-admin sessions to activity invalidation', () => {
		const events = createLibraryActivityEvents(mux);
		events.start(false, 'user-1');
		expect(FakeEventSource.instances).toHaveLength(1);
		expect(h.invalidate).not.toHaveBeenCalled();
		FakeEventSource.instances[0].emit(
			'activity.changed',
			revisionEvent({ scan: 1, identification: 2, operation: 3, catalog: 4 })
		);
		h.invalidate.mockClear();

		FakeEventSource.instances[0].emit(
			'activity.changed',
			revisionEvent({ scan: 2, identification: 2, operation: 3, catalog: 4 })
		);

		expect(h.invalidate).toHaveBeenCalledOnce();
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.activityPrefix()
		});
	});

	it('stays silent for non-admin sessions on reconnect', () => {
		const events = createLibraryActivityEvents(mux);
		events.start(false, 'user-1');
		FakeEventSource.instances[0].emitOpen();
		expect(h.invalidate).not.toHaveBeenCalled();
	});

	it('defers the admin refresh to the first open when starting disconnected', () => {
		const idle = createMuxEventStream();
		const events = createLibraryActivityEvents(idle);
		events.start(true, 'user-1');
		expect(h.invalidate).not.toHaveBeenCalled();
		idle.connect();
		expect(FakeEventSource.instances).toHaveLength(2);
		FakeEventSource.instances[1].emitOpen();
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.operationsPrefix()
		});
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.reviewsPrefix()
		});
		idle.disconnect();
	});

	it('unregisters and resets revision state for the next session', () => {
		const events = createLibraryActivityEvents(mux);
		events.start(true, 'user-1');
		FakeEventSource.instances[0].emit(
			'activity.changed',
			revisionEvent({ scan: 1, identification: 2, operation: 3, catalog: 4 })
		);

		events.start(false, 'user-1');
		expect(FakeEventSource.instances).toHaveLength(1);
		h.invalidate.mockClear();
		FakeEventSource.instances[0].emit(
			'activity.changed',
			revisionEvent({ scan: 2, identification: 2, operation: 3, catalog: 4 })
		);
		expect(h.invalidate).not.toHaveBeenCalled();

		events.stop();
		FakeEventSource.instances[0].emit(
			'activity.changed',
			revisionEvent({ scan: 3, identification: 2, operation: 3, catalog: 4 })
		);
		expect(h.invalidate).not.toHaveBeenCalled();
	});

	it('ignores malformed revision payloads', () => {
		const events = createLibraryActivityEvents(mux);
		events.start(false, 'user-1');
		FakeEventSource.instances[0].emit('activity.changed', new MessageEvent('activity.changed'));
		FakeEventSource.instances[0].emit(
			'activity.changed',
			new MessageEvent('activity.changed', { data: '{"revisions":{"scan":"one"}}' })
		);
		expect(h.invalidate).not.toHaveBeenCalled();
		expect(h.invalidateCatalog).not.toHaveBeenCalled();
	});

	it('invalidates when the first SSE revision is newer than the HTTP snapshot', () => {
		const events = createLibraryActivityEvents(mux);
		events.start(false, 'user-1');
		FakeEventSource.instances[0].emit(
			'activity.changed',
			revisionEvent({ scan: 2, identification: 2, operation: 3, catalog: 4 })
		);
		expect(h.invalidate).not.toHaveBeenCalled();

		h.activityData = {
			revisions: { scan: 1, identification: 2, operation: 3, catalog: 4 }
		};
		h.queryCacheListener?.();

		expect(h.invalidate).toHaveBeenCalledOnce();
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.activityPrefix()
		});
	});
});

function revisionEvent(revisions: Record<string, number>): MessageEvent<string> {
	return new MessageEvent('activity.changed', { data: JSON.stringify({ revisions }) });
}
