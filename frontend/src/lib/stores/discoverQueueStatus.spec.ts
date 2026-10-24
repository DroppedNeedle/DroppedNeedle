import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

const { apiMock, authMock, scope } = vi.hoisted(() => ({
	apiMock: { global: { get: vi.fn(), post: vi.fn() } },
	authMock: { user: { id: 'u1' } },
	scope: { source: 'a' }
}));
vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$lib/api/client', () => ({ api: apiMock, ApiError: class extends Error {} }));
vi.mock('$lib/stores/authStore.svelte', () => ({ authStore: authMock }));
vi.mock('$lib/stores/cacheTtl.svelte', () => ({
	getCacheTTLs: () => ({ discoverQueuePollingInterval: 1000 })
}));
vi.mock('$lib/queries/musicbrainz/sourceScope.svelte', () => ({
	musicBrainzSourceKey: () => scope.source
}));
import { discoverQueueStatusStore as status } from './discoverQueueStatus';

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (reason: Error) => void;
	const promise = new Promise<T>((yes, no) => {
		resolve = yes;
		reject = no;
	});
	return { promise, resolve, reject };
}

beforeEach(() => {
	vi.useFakeTimers();
	vi.clearAllMocks();
	apiMock.global.get.mockReset();
	apiMock.global.post.mockReset();
	status.reset();
	authMock.user.id = 'u1';
	scope.source = 'a';
});
afterEach(() => {
	status.reset();
	vi.useRealTimers();
});

describe('queue generation owns subsequent status observations', () => {
	it('does not overlap slow poll GETs and still adopts their result', async () => {
		const slow = deferred<{ status: string }>();
		apiMock.global.get.mockReturnValueOnce(slow.promise).mockResolvedValue({ status: 'idle' });
		status.startPolling();
		await vi.advanceTimersByTimeAsync(4000);
		expect(apiMock.global.get).toHaveBeenCalledOnce();
		slow.resolve({ status: 'building' });
		await vi.advanceTimersByTimeAsync(1000);
		expect(apiMock.global.get).toHaveBeenCalledTimes(2);
		expect(get(status).status).toBe('idle');
		expect(status.isPolling).toBe(false);
	});

	it('does not restart polling if a subscriber resets during status publication', async () => {
		const unsubscribe = status.subscribe((value) => {
			if (value.status === 'building') status.reset();
		});
		apiMock.global.get.mockResolvedValue({ status: 'building' });
		await status.init();
		expect(get(status).status).toBe('unknown');
		expect(status.isPolling).toBe(false);
		unsubscribe();
	});
	it('settles a current failed POST and releases its pending request ownership', async () => {
		apiMock.global.post.mockRejectedValue(new Error('current failure'));
		await status.triggerGenerate();
		expect(get(status).status).toBe('error');
		expect(status.isPolling).toBe(false);
		apiMock.global.get.mockResolvedValue({ status: 'idle' });
		await status.fetchStatus();
		expect(get(status).status).toBe('idle');
		expect(apiMock.global.post).toHaveBeenCalledOnce();
	});
	it('rejects an old idle GET and suppresses new polling while generation is pending', async () => {
		const old = deferred<{ status: string }>();
		const generation = deferred<{ status: string }>();
		apiMock.global.get.mockReturnValue(old.promise);
		apiMock.global.post.mockReturnValue(generation.promise);
		const poll = status.fetchStatus();
		const build = status.triggerGenerate(true);
		status.startPolling();
		expect(status.isPolling).toBe(false);
		expect(await status.fetchStatus()).toBeNull();
		expect(apiMock.global.get).toHaveBeenCalledOnce();
		old.resolve({ status: 'idle' });
		expect(await poll).toBeNull();
		expect(get(status).status).toBe('building');
		generation.resolve({ status: 'building' });
		await build;
		expect(status.isPolling).toBe(true);
		expect(apiMock.global.post).toHaveBeenCalledExactlyOnceWith('/api/v1/discover/queue/generate', {
			force: true
		});
	});

	it('lets only the latest overlapping generation publish, including errors and pending cleanup', async () => {
		const first = deferred<{ status: string }>();
		const second = deferred<{ status: string }>();
		apiMock.global.post.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
		const oldBuild = status.triggerGenerate(false);
		const newBuild = status.triggerGenerate(true);
		first.reject(new Error('old failure'));
		await oldBuild;
		expect(get(status).status).toBe('building');
		expect(await status.fetchStatus()).toBeNull();
		expect(apiMock.global.get).not.toHaveBeenCalled();
		second.resolve({ status: 'idle' });
		await newBuild;
		expect(get(status).status).toBe('idle');
		expect(status.isPolling).toBe(false);
		expect(apiMock.global.post.mock.calls.map(([, body]) => body)).toEqual([
			{ force: false },
			{ force: true }
		]);
	});

	it('ignores an older generation response arriving after the new generation', async () => {
		const first = deferred<{ status: string }>();
		apiMock.global.post
			.mockReturnValueOnce(first.promise)
			.mockResolvedValueOnce({ status: 'building' });
		const oldBuild = status.triggerGenerate(false);
		await status.triggerGenerate(true);
		first.resolve({ status: 'idle' });
		await oldBuild;
		expect(get(status).status).toBe('building');
		expect(status.isPolling).toBe(true);
	});

	it('ignores an older overlapping GET after a newer GET response', async () => {
		const first = deferred<{ status: string }>();
		apiMock.global.get
			.mockReturnValueOnce(first.promise)
			.mockResolvedValueOnce({ status: 'ready', queue_id: 'new' });
		const oldPoll = status.fetchStatus();
		await status.fetchStatus();
		first.resolve({ status: 'idle' });
		expect(await oldPoll).toBeNull();
		expect(get(status).queueId).toBe('new');
	});

	it('an old poll continuation cannot stop polling owned by the new generation', async () => {
		const old = deferred<{ status: string }>();
		apiMock.global.get.mockReturnValueOnce(old.promise).mockResolvedValue({ status: 'building' });
		status.startPolling();
		await vi.advanceTimersByTimeAsync(1000);
		apiMock.global.post.mockResolvedValue({ status: 'building' });
		await status.triggerGenerate(true);
		old.resolve({ status: 'idle' });
		await vi.advanceTimersByTimeAsync(1000);
		expect(status.isPolling).toBe(true);
		expect(get(status).status).toBe('building');
		expect(apiMock.global.get).toHaveBeenCalledTimes(2);
	});

	it.each(['reset', 'user', 'source'] as const)(
		'rejects a generation after %s invalidation',
		async (change) => {
			const pending = deferred<{ status: string; queue_id: string }>();
			apiMock.global.post.mockReturnValue(pending.promise);
			const build = status.triggerGenerate();
			if (change === 'reset') status.reset();
			else if (change === 'user') authMock.user.id = 'u2';
			else scope.source = 'b';
			pending.resolve({ status: 'ready', queue_id: 'old' });
			await build;
			expect(get(status).queueId).toBeUndefined();
			expect(status.isPolling).toBe(false);
			apiMock.global.get.mockResolvedValue({ status: 'idle' });
			await status.fetchStatus();
			expect(get(status).status).toBe('idle');
		}
	);

	it.each(['reset', 'user', 'source'] as const)(
		'rejects a GET after %s invalidation',
		async (change) => {
			const pending = deferred<{ status: string; queue_id: string }>();
			apiMock.global.get.mockReturnValueOnce(pending.promise);
			const poll = status.fetchStatus();
			if (change === 'reset') status.reset();
			else if (change === 'user') authMock.user.id = 'u2';
			else scope.source = 'b';
			pending.resolve({ status: 'ready', queue_id: 'old' });
			expect(await poll).toBeNull();
			expect(get(status).queueId).toBeUndefined();
		}
	);
});
