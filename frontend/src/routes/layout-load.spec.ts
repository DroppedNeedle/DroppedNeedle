import { beforeEach, describe, expect, it, vi } from 'vitest';

type TestUser = {
	id: string;
	display_name: string;
	role: 'admin' | 'trusted' | 'user';
	email: string | null;
	avatar_url: string | null;
	username: string | null;
	username_display: string | null;
	providers: string[];
};

const state = vi.hoisted(() => ({
	apiGet: vi.fn(),
	user: null as TestUser | null,
	initialized: false,
	clear: vi.fn(),
	markInitialized: vi.fn()
}));

vi.mock('$app/environment', () => ({ browser: false }));
vi.mock('$lib/api/client', () => {
	class ApiError extends Error {
		constructor(
			readonly status: number,
			message: string
		) {
			super(message);
		}
	}
	return { ApiError, api: { global: { get: state.apiGet } } };
});
vi.mock('$lib/constants', () => ({
	AUTH_FREE_PATHS: ['/login', '/setup'],
	API: {
		auth: { setupStatus: () => '/setup-status', me: () => '/me' },
		me: { scrobblePreferences: () => '/scrobble-preferences' }
	}
}));
vi.mock('$lib/queries/QueryClient', () => ({ resetQueryCacheForUserSwitch: vi.fn() }));
vi.mock('$lib/stores/musicSource', () => ({
	DEFAULT_SOURCE: 'listenbrainz',
	isMusicSource: vi.fn(() => false),
	musicSourceStore: { reset: vi.fn() }
}));
vi.mock('$lib/stores/scrobble.svelte', () => ({
	scrobbleManager: { reset: vi.fn(), refreshSettings: vi.fn() }
}));
vi.mock('$lib/utils/userScopedCaches', () => ({ clearUserScopedLocalCaches: vi.fn() }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: {
		get user() {
			return state.user;
		},
		get initialized() {
			return state.initialized;
		},
		get isAuthenticated() {
			return state.user !== null;
		},
		setUser(user: TestUser) {
			state.user = user;
		},
		clear() {
			state.clear();
			state.user = null;
		},
		markInitialized() {
			state.markInitialized();
			state.initialized = true;
		}
	}
}));

import { ApiError } from '$lib/api/client';
import { load } from './+layout';

const user: TestUser = {
	id: 'user-1',
	display_name: 'Test User',
	role: 'user',
	email: null,
	avatar_url: null,
	username: 'test',
	username_display: 'test',
	providers: ['local']
};

function loadPage() {
	return load({ url: new URL('http://localhost/') } as Parameters<typeof load>[0]);
}

describe('+layout load session bootstrap', () => {
	beforeEach(() => {
		state.apiGet.mockReset();
		state.clear.mockReset();
		state.markInitialized.mockReset();
		state.user = null;
		state.initialized = false;
	});

	it('keeps the session intact and reports a busy server when /auth/me times out', async () => {
		state.user = user;
		state.apiGet
			.mockResolvedValueOnce({ required: false })
			.mockRejectedValueOnce(new DOMException('Timed out', 'TimeoutError'));

		await expect(loadPage()).rejects.toMatchObject({
			status: 503,
			body: { message: 'The server is busy. Your session is safe - try again shortly.' }
		});
		expect(state.clear).not.toHaveBeenCalled();
		expect(state.user).toBe(user);
		expect(state.apiGet).toHaveBeenNthCalledWith(1, '/setup-status', { timeoutMs: 10_000 });
		expect(state.apiGet).toHaveBeenNthCalledWith(2, '/me', { timeoutMs: 10_000 });
	});

	it('clears the session only for an actual 401 response', async () => {
		state.user = user;
		state.apiGet
			.mockResolvedValueOnce({ required: false })
			.mockRejectedValueOnce(new ApiError(401, 'Unauthorized'));

		await expect(loadPage()).rejects.toMatchObject({ status: 302, location: '/login' });
		expect(state.clear).toHaveBeenCalledOnce();
		expect(state.markInitialized).toHaveBeenCalledOnce();
		expect(state.user).toBeNull();
	});

	it('bounds the optional preferences request without discarding the session', async () => {
		state.user = user;
		state.initialized = true;
		state.apiGet
			.mockResolvedValueOnce({ required: false })
			.mockRejectedValueOnce(new DOMException('Timed out', 'TimeoutError'));

		await expect(loadPage()).resolves.toEqual({ primarySource: 'listenbrainz' });
		expect(state.apiGet).toHaveBeenNthCalledWith(2, '/scrobble-preferences', {
			timeoutMs: 10_000
		});
		expect(state.clear).not.toHaveBeenCalled();
		expect(state.user).toBe(user);
	});
});
