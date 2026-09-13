import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

class FakeEventSource {
	static instances: FakeEventSource[] = [];
	url: string;
	listeners = new Map<string, Set<(e: Event) => void>>();
	onopen: ((event: Event) => void) | null = null;
	constructor(url: string) {
		this.url = url;
		FakeEventSource.instances.push(this);
	}
	addEventListener(type: string, cb: (e: Event) => void) {
		const set = this.listeners.get(type) ?? new Set<(e: Event) => void>();
		set.add(cb);
		this.listeners.set(type, set);
	}
	removeEventListener(type: string, cb: (e: Event) => void) {
		this.listeners.get(type)?.delete(cb);
	}
	close() {}
	emit(type: string, data: unknown) {
		const event = { data: JSON.stringify(data) } as MessageEvent;
		for (const listener of this.listeners.get(type) ?? []) listener(event);
	}
	emitOpen() {
		this.onopen?.(new Event('open'));
	}
}

vi.stubGlobal('EventSource', FakeEventSource);
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));
vi.mock('$lib/queries/QueryClient', () => ({ invalidateQueriesWithPersister: vi.fn() }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } }
}));

import { toastStore } from '$lib/stores/toast';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { FollowQueryKeyFactory } from './FollowQueryKeyFactory';
import { WantedQueryKeyFactory } from '$lib/queries/wanted/WantedQueryKeyFactory';
import { PlaylistQueryKeyFactory } from '$lib/queries/playlists/PlaylistQueryKeyFactory';
import { DropImportQueryKeyFactory } from '$lib/queries/import/DropImportQueryKeyFactory';
import { FreeMusicQueryKeyFactory } from '$lib/queries/free-music/FreeMusicQueryKeyFactory';
import { LOCAL_KEYS } from '$lib/queries/local/LocalQueries.svelte';
import { createMuxEventStream, type MuxEventStream } from '$lib/queries/events/MuxEventStream';
import { createFollowingEvents } from './FollowingEvents';

const mockShow = vi.mocked(toastStore.show);
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

let mux: MuxEventStream;

beforeEach(() => {
	vi.clearAllMocks();
	FakeEventSource.instances = [];
	mux = createMuxEventStream();
	mux.connect();
	// FollowingEvents persists its seen-id de-dupe sets to sessionStorage; in
	// environments where a real (non-jsdom) sessionStorage global is present
	// (e.g. Node's built-in Web Storage), state leaks across tests/files
	// unless cleared - reset it so each test starts from a clean de-dupe state.
	if (typeof sessionStorage !== 'undefined') sessionStorage.clear();
});

afterEach(() => {
	mux.disconnect();
});

describe('FollowingEvents', () => {
	it('toasts once per enqueue and ignores the replayed snapshot', () => {
		const fe = createFollowingEvents(mux);
		fe.start();
		const es = FakeEventSource.instances[0];

		es.emit('auto_download_enqueued', { task_id: 'X', title: 'Album X' });
		expect(mockShow).toHaveBeenCalledTimes(1);
		expect(mockShow).toHaveBeenCalledWith(
			expect.objectContaining({ message: expect.stringContaining('Album X'), type: 'info' })
		);

		es.emit('auto_download_enqueued', { task_id: 'X', title: 'Album X' });
		expect(mockShow).toHaveBeenCalledTimes(1);

		es.emit('auto_download_enqueued', { task_id: 'Y', title: 'Album Y' });
		expect(mockShow).toHaveBeenCalledTimes(2);
	});

	it('ignores events without a task id', () => {
		const fe = createFollowingEvents(mux);
		fe.start();
		FakeEventSource.instances[0].emit('auto_download_enqueued', { title: 'No id' });
		expect(mockShow).not.toHaveBeenCalled();
		expect(mockInvalidate).not.toHaveBeenCalled();
	});

	it('refreshes the sidebar badge count once per real enqueue', () => {
		const fe = createFollowingEvents(mux);
		fe.start();
		const es = FakeEventSource.instances[0];

		es.emit('auto_download_enqueued', { task_id: 'X', title: 'Album X' });
		expect(mockInvalidate).toHaveBeenCalledTimes(1);
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: FollowQueryKeyFactory.newReleasesUnseen('userA')
		});

		// replayed snapshot is de-duped - no second invalidation
		es.emit('auto_download_enqueued', { task_id: 'X', title: 'Album X' });
		expect(mockInvalidate).toHaveBeenCalledTimes(1);
	});

	it('revalidates badge counts on reconnect', () => {
		const fe = createFollowingEvents(mux);
		fe.start();
		mockInvalidate.mockClear();

		FakeEventSource.instances[0].emitOpen();

		expect(mockInvalidate).toHaveBeenCalledTimes(8);
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: FollowQueryKeyFactory.newReleasesUnseen('userA')
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: WantedQueryKeyFactory.list('userA')
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: FollowQueryKeyFactory.concertsUnseen('userA')
		});
		// user-scoped parent cascades to detail, unlike list()
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: [...PlaylistQueryKeyFactory.prefix, 'userA']
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: FollowQueryKeyFactory.concerts('userA')
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: DropImportQueryKeyFactory.prefix
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: FreeMusicQueryKeyFactory.prefix
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: LOCAL_KEYS.root
		});
	});

	it('opens no stream of its own and goes silent after stop', () => {
		const fe = createFollowingEvents(mux);
		fe.start();
		expect(FakeEventSource.instances).toHaveLength(1);
		expect(FakeEventSource.instances[0].url).toBe('/api/v1/events/stream');

		fe.stop();
		FakeEventSource.instances[0].emit('auto_download_enqueued', {
			task_id: 'Z',
			title: 'Album Z'
		});
		FakeEventSource.instances[0].emitOpen();
		expect(mockShow).not.toHaveBeenCalled();
		expect(mockInvalidate).not.toHaveBeenCalled();
	});
});
