import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	invalidate: vi.fn().mockResolvedValue(undefined),
	toast: vi.fn(),
	activity: undefined as { revisions: Record<string, number> } | undefined
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: h.invalidate,
	queryClient: {
		getQueryData: () => h.activity,
		getQueryCache: () => ({ subscribe: () => () => undefined })
	}
}));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: h.toast } }));

import type { MuxEventListener, MuxEventStream } from './MuxEventStream';
import { createEventDispatcher, EVENT_TABLE } from './dispatcher';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { RequestQueryKeyFactory } from '$lib/queries/requests/RequestQueryKeyFactory';

const here = dirname(fileURLToPath(import.meta.url));
const contract = JSON.parse(
	readFileSync(
		join(here, '..', '..', '..', '..', '..', 'server', 'openapi', 'openapi.json'),
		'utf8'
	)
) as {
	components: {
		schemas: { StreamEvent: { oneOf: { properties: { event: { enum: string[] } } }[] } };
	};
};

function fakeMux() {
	const listeners = new Map<string, Set<MuxEventListener>>();
	const connects = new Set<() => void>();
	const mux: MuxEventStream = {
		connect: () => undefined,
		disconnect: () => undefined,
		on(name, listener) {
			let set = listeners.get(name);
			if (!set) listeners.set(name, (set = new Set()));
			set.add(listener);
			return () => set.delete(listener);
		},
		onConnect(listener) {
			connects.add(listener);
			return () => connects.delete(listener);
		},
		isConnected: false
	};
	return {
		mux,
		emit(name: string, data: unknown) {
			const event = new MessageEvent(name, { data: JSON.stringify(data) });
			for (const listener of listeners.get(name) ?? []) listener(event);
		},
		open() {
			for (const listener of connects) listener();
		}
	};
}

function swept(queryKey: readonly unknown[]): boolean {
	return h.invalidate.mock.calls.some(([filter]) =>
		filter.predicate
			? filter.predicate({ queryKey })
			: (filter.queryKey as unknown[]).every(
					(part, i) => JSON.stringify(part) === JSON.stringify(queryKey[i])
				)
	);
}

const revisions = (catalog: number, operation = 1) => ({
	id: `activity:${catalog}-${operation}`,
	revisions: { scan: 1, identification: 1, operation, catalog }
});

beforeEach(() => {
	vi.clearAllMocks();
	h.activity = undefined;
});

afterEach(() => {
	vi.useRealTimers();
});

describe('event dispatcher', () => {
	it('handles exactly the events the server sends', () => {
		const sent = contract.components.schemas.StreamEvent.oneOf.flatMap(
			(member) => member.properties.event.enum
		);
		expect(Object.keys(EVENT_TABLE).sort()).toEqual([...new Set(sent)].sort());
	});

	it('refreshes the download and request queries on downloads.changed', () => {
		const { mux, emit } = fakeMux();
		const events = createEventDispatcher(mux);
		events.start({ userId: 'user-1', isAdmin: false });
		emit('downloads.changed', { id: 'downloads:1' });
		expect(swept(DownloadQueryKeyFactory.tasks('user-1'))).toBe(true);
		expect(swept(DownloadQueryKeyFactory.activity('user-1'))).toBe(true);
		expect(swept(RequestQueryKeyFactory.active('user-1'))).toBe(true);
	});

	it('toasts once per id, through replays and restarts', () => {
		const { mux, emit } = fakeMux();
		const events = createEventDispatcher(mux);
		const notice = { event_id: 'e1', album_title: 'Blue', artist_name: 'A' };
		events.start({ userId: 'user-1', isAdmin: false });
		emit('wanted_fulfilled', notice);
		emit('wanted_fulfilled', notice);
		events.start({ userId: 'user-1', isAdmin: false });
		emit('wanted_fulfilled', notice);
		expect(h.toast).toHaveBeenCalledOnce();
		expect(h.toast).toHaveBeenCalledWith({
			message: 'Blue is now in your library.',
			type: 'success'
		});
		events.stop();
		emit('wanted_fulfilled', { ...notice, event_id: 'e2' });
		expect(h.toast).toHaveBeenCalledOnce();
	});

	it('refreshes library activity, and the admin views, when a revision moves', () => {
		const { mux, emit } = fakeMux();
		const events = createEventDispatcher(mux);
		events.start({ userId: 'user-1', isAdmin: false });
		emit('activity.changed', revisions(4, 3));
		h.invalidate.mockClear();
		emit('activity.changed', revisions(4, 4));
		expect(h.invalidate).toHaveBeenCalledOnce();
		expect(swept(LibraryQueryKeyFactory.activity('user-1'))).toBe(true);

		events.start({ userId: 'admin-1', isAdmin: true });
		emit('activity.changed', revisions(4, 4));
		h.invalidate.mockClear();
		emit('activity.changed', revisions(4, 5));
		expect(swept(LibraryQueryKeyFactory.activity('admin-1'))).toBe(true);
		expect(swept(LibraryQueryKeyFactory.currentRuns())).toBe(true);
		expect(swept(LibraryQueryKeyFactory.reviewsPrefix())).toBe(true);
		expect(swept(['admin', 'library-management', 'operations'])).toBe(true);
		expect(swept(['user:admin-1', 'artist'])).toBe(false);
	});

	it('refreshes the catalog at most once per 10 s window, with a trailing refresh', () => {
		vi.useFakeTimers();
		const { mux, emit } = fakeMux();
		const events = createEventDispatcher(mux);
		events.start({ userId: 'user-1', isAdmin: false });
		const catalogSweeps = () =>
			h.invalidate.mock.calls.filter(([filter]) =>
				filter.predicate?.({ queryKey: ['user:user-1', 'artist'] })
			).length;
		emit('activity.changed', revisions(1));
		emit('activity.changed', revisions(2));
		vi.advanceTimersByTime(1000);
		emit('activity.changed', revisions(3));
		expect(catalogSweeps()).toBe(1);
		vi.advanceTimersByTime(9000);
		expect(catalogSweeps()).toBe(2);

		emit('activity.changed', revisions(4));
		emit('activity.changed', revisions(5));
		events.stop();
		vi.advanceTimersByTime(60_000);
		expect(catalogSweeps()).toBe(2);
	});

	it('ignores malformed frames and still feeds live views', () => {
		const { mux, emit } = fakeMux();
		const events = createEventDispatcher(mux);
		const live = vi.fn();
		events.on('download_progress', live);
		events.start({ userId: 'user-1', isAdmin: false });
		emit('activity.changed', { id: 'a', revisions: { scan: 'one' } });
		emit('activity.changed', { id: 'b', revisions: { scan: 'two' } });
		expect(h.invalidate).not.toHaveBeenCalled();
		emit('download_progress', { task_id: 't1' });
		expect(live).toHaveBeenCalledWith({ task_id: 't1' });
	});

	it('refreshes what a hidden tab may have missed on every reconnect', () => {
		const { mux, open } = fakeMux();
		const events = createEventDispatcher(mux);
		events.start({ userId: 'user-1', isAdmin: false });
		expect(h.invalidate).not.toHaveBeenCalled();
		open();
		expect(swept(['user:user-1', 'following', 'new-releases-unseen'])).toBe(true);
		expect(swept(['user:user-1', 'wanted', 'list'])).toBe(true);
		expect(swept(['user:user-1', 'playlists'])).toBe(true);
		expect(swept(DownloadQueryKeyFactory.searchJob('user-1', 'job-1'))).toBe(true);
		expect(swept(LibraryQueryKeyFactory.currentRuns())).toBe(false);
	});
});
