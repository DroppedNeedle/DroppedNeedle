import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { createMuxEventStream } from './MuxEventStream';

class FakeEventSource {
	static instances: FakeEventSource[] = [];
	readonly url: string;
	onopen: ((event: Event) => void) | null = null;
	readonly listeners = new Map<string, Set<(event: Event) => void>>();
	closed = false;

	constructor(url: string) {
		this.url = url;
		FakeEventSource.instances.push(this);
	}

	addEventListener(type: string, listener: EventListener): void {
		const callback = listener as (event: Event) => void;
		const set = this.listeners.get(type) ?? new Set<(event: Event) => void>();
		set.add(callback);
		this.listeners.set(type, set);
	}

	removeEventListener(type: string, listener: EventListener): void {
		this.listeners.get(type)?.delete(listener as (event: Event) => void);
	}

	close(): void {
		this.closed = true;
	}

	emitOpen(): void {
		this.onopen?.(new Event('open'));
	}

	emit(type: string, data: unknown): void {
		const event = new MessageEvent(type, { data: JSON.stringify(data) });
		for (const listener of this.listeners.get(type) ?? []) listener(event);
	}
}

function stubDocument(hidden: boolean) {
	const doc = {
		hidden,
		addEventListener: vi.fn(),
		removeEventListener: vi.fn()
	};
	vi.stubGlobal('document', doc);
	return doc;
}

function visibilityHandler(doc: { addEventListener: ReturnType<typeof vi.fn> }) {
	const call = doc.addEventListener.mock.calls.find(([type]) => type === 'visibilitychange');
	if (!call) throw new Error('visibilitychange listener was not registered');
	return call[1] as () => void;
}

beforeEach(() => {
	FakeEventSource.instances = [];
	vi.stubGlobal('EventSource', FakeEventSource);
});

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('createMuxEventStream', () => {
	it('opens exactly one stream to the mux endpoint and ignores repeat connects', () => {
		const mux = createMuxEventStream();
		mux.connect();
		mux.connect();

		expect(FakeEventSource.instances).toHaveLength(1);
		expect(FakeEventSource.instances[0].url).toBe('/api/v3/events/stream');
		expect(mux.isConnected).toBe(true);
		mux.disconnect();
	});

	it('delivers named events to registered listeners and honors unsubscribe', () => {
		const mux = createMuxEventStream();
		mux.connect();
		const seen: unknown[] = [];
		const unsub = mux.on('wanted_new_candidates', (event) => {
			seen.push(JSON.parse((event as MessageEvent).data));
		});

		FakeEventSource.instances[0].emit('wanted_new_candidates', { count: 1 });
		expect(seen).toEqual([{ count: 1 }]);

		unsub();
		FakeEventSource.instances[0].emit('wanted_new_candidates', { count: 2 });
		expect(seen).toEqual([{ count: 1 }]);
		mux.disconnect();
	});

	it('hibernates the stream while hidden and reopens when visible again', () => {
		const doc = stubDocument(false);
		const mux = createMuxEventStream();
		mux.connect();
		expect(FakeEventSource.instances).toHaveLength(1);

		doc.hidden = true;
		visibilityHandler(doc)();
		expect(FakeEventSource.instances[0].closed).toBe(true);
		expect(mux.isConnected).toBe(false);

		doc.hidden = false;
		visibilityHandler(doc)();
		expect(FakeEventSource.instances).toHaveLength(2);
		expect(mux.isConnected).toBe(true);
		mux.disconnect();
	});

	it('reattaches listeners to the reopened stream after hibernation', () => {
		const doc = stubDocument(false);
		const mux = createMuxEventStream();
		const seen: unknown[] = [];
		mux.on('snapshot', (event) => {
			seen.push(JSON.parse((event as MessageEvent).data));
		});
		mux.connect();

		doc.hidden = true;
		visibilityHandler(doc)();
		doc.hidden = false;
		visibilityHandler(doc)();

		FakeEventSource.instances[1].emit('snapshot', { sessions: [] });
		expect(seen).toEqual([{ sessions: [] }]);
		mux.disconnect();
	});

	it('stays down across visibility flips after disconnect', () => {
		const doc = stubDocument(false);
		const mux = createMuxEventStream();
		mux.connect();
		mux.disconnect();

		doc.hidden = true;
		visibilityHandler(doc)();
		doc.hidden = false;
		visibilityHandler(doc)();

		expect(FakeEventSource.instances).toHaveLength(1);
		expect(mux.isConnected).toBe(false);
	});
});
