import { describe, it, expect, vi, beforeEach } from 'vitest';

class FakeEventSource {
	static instances: FakeEventSource[] = [];
	url: string;
	listeners: Record<string, (e: Event) => void> = {};
	constructor(url: string) {
		this.url = url;
		FakeEventSource.instances.push(this);
	}
	addEventListener(type: string, cb: (e: Event) => void) {
		this.listeners[type] = cb;
	}
	close() {}
	emit(type: string, data: unknown) {
		this.listeners[type]?.({ data: JSON.stringify(data) } as MessageEvent);
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
import { createFollowingEvents } from './FollowingEvents';

const mockShow = vi.mocked(toastStore.show);
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

beforeEach(() => {
	vi.clearAllMocks();
	FakeEventSource.instances = [];
	// FollowingEvents persists its seen-id de-dupe sets to sessionStorage; in
	// environments where a real (non-jsdom) sessionStorage global is present
	// (e.g. Node's built-in Web Storage), state leaks across tests/files
	// unless cleared - reset it so each test starts from a clean de-dupe state.
	if (typeof sessionStorage !== 'undefined') sessionStorage.clear();
});

describe('FollowingEvents', () => {
	it('toasts once per enqueue and ignores the replayed snapshot', () => {
		const fe = createFollowingEvents();
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
		const fe = createFollowingEvents();
		fe.start();
		FakeEventSource.instances[0].emit('auto_download_enqueued', { title: 'No id' });
		expect(mockShow).not.toHaveBeenCalled();
		expect(mockInvalidate).not.toHaveBeenCalled();
	});

	it('refreshes the sidebar badge count once per real enqueue', () => {
		const fe = createFollowingEvents();
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
});
