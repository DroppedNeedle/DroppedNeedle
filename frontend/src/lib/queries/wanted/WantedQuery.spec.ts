import { describe, expect, it, vi, beforeEach } from 'vitest';

vi.mock('@tanstack/svelte-query', async (importOriginal) => {
	const actual = await importOriginal<typeof import('@tanstack/svelte-query')>();
	return {
		...actual,
		createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
		createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
	};
});

vi.mock('idb-keyval', () => ({
	get: vi.fn(),
	set: vi.fn(),
	del: vi.fn(),
	entries: vi.fn(async () => []),
	clear: vi.fn()
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { get: vi.fn(), post: vi.fn(), put: vi.fn(), delete: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null, isAdmin: false },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { queryClient } from '../QueryClient';
import { WantedQueryKeyFactory } from './WantedQueryKeyFactory';
import { WANTED_ENDPOINTS } from './endpoints';
import { getWantedWatchesQuery } from './WantedQuery.svelte';
import {
	createMarkWantedSeenMutation,
	createResumeWatchMutation,
	createStopWatchMutation,
	type WantedActionVars
} from './WantedMutations.svelte';

const mockGet = vi.mocked(api.global.get);
const mockPost = vi.mocked(api.global.post);
const mockShow = vi.mocked(toastStore.show);

type Opts = {
	queryKey?: unknown;
	enabled?: boolean;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	mutationFn?: (vars: WantedActionVars) => Promise<unknown>;
	onSuccess?: (data: unknown, vars: WantedActionVars) => void;
	onError?: (err: unknown, vars: WantedActionVars) => void;
	onSettled?: () => void;
};

const MBID = '22222222-2222-2222-2222-222222222222';
const VARS: WantedActionVars = { mbid: MBID, albumTitle: 'the arrival' };
const auth = authStore as unknown as { user: { id: string } | null; isAdmin: boolean };

beforeEach(() => {
	vi.clearAllMocks();
	auth.user = { id: 'userA' };
	mockGet.mockResolvedValue({ items: [], count: 0, retrying: [] });
	mockPost.mockResolvedValue({ success: true, state: 'stopped' });
	queryClient.clear();
});

describe('WantedQueryKeyFactory', () => {
	it('scopes the list key by userId and falls back to anon', () => {
		expect(WantedQueryKeyFactory.list('userA')).toEqual(['wanted', 'list', 'userA']);
		expect(WantedQueryKeyFactory.list(undefined)).toEqual(['wanted', 'list', 'anon']);
		expect(WantedQueryKeyFactory.list('userB')).not.toEqual(WantedQueryKeyFactory.list('userA'));
	});
});

describe('getWantedWatchesQuery', () => {
	it('hits the list endpoint with a user-scoped key and forwards the signal', async () => {
		const opts = getWantedWatchesQuery(() => true) as unknown as Opts;
		expect(opts.queryKey).toEqual(['wanted', 'list', 'userA']);
		expect(opts.enabled).toBe(true);
		const signal = new AbortController().signal;
		await opts.queryFn!({ signal });
		expect(mockGet.mock.calls[0][0]).toBe(WANTED_ENDPOINTS.list());
		expect(mockGet.mock.calls[0][1]).toEqual({ signal });
	});

	it('stays disabled while the tab is closed or nobody is signed in', () => {
		expect((getWantedWatchesQuery(() => false) as unknown as Opts).enabled).toBe(false);
		auth.user = null;
		expect((getWantedWatchesQuery(() => true) as unknown as Opts).enabled).toBe(false);
	});
});

describe('wanted mutations', () => {
	it('stop posts to the stop endpoint and toasts on success', async () => {
		const opts = createStopWatchMutation() as unknown as Opts;
		await opts.mutationFn!(VARS);
		expect(mockPost.mock.calls[0][0]).toBe(WANTED_ENDPOINTS.stop(MBID));
		opts.onSuccess?.({ success: true, state: 'stopped' }, VARS);
		expect(mockShow).toHaveBeenCalledWith(
			expect.objectContaining({ message: expect.stringContaining('the arrival') })
		);
	});

	it('stop toasts an error on failure', () => {
		const opts = createStopWatchMutation() as unknown as Opts;
		opts.onError?.(new Error('nope'), VARS);
		expect(mockShow).toHaveBeenCalledWith(expect.objectContaining({ type: 'error' }));
	});

	it('resume posts to the resume endpoint', async () => {
		const opts = createResumeWatchMutation() as unknown as Opts;
		await opts.mutationFn!(VARS);
		expect(mockPost.mock.calls[0][0]).toBe(WANTED_ENDPOINTS.resume(MBID));
	});

	it('mark-seen posts to the seen endpoint without a toast', async () => {
		const opts = createMarkWantedSeenMutation() as unknown as Opts;
		await opts.mutationFn!(VARS);
		expect(mockPost.mock.calls[0][0]).toBe(WANTED_ENDPOINTS.seen(MBID));
		expect(mockShow).not.toHaveBeenCalled();
	});
});
