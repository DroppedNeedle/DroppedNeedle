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
	setupRequired: false,
	clear: vi.fn(),
	markInitialized: vi.fn(),
	ensureQueryData: vi.fn().mockResolvedValue({ primary_music_source: 'invalid' }),
	resetQueryCache: vi.fn(),
	sessionCleanup: vi.fn(),
	musicSourceReset: vi.fn(),
	musicSourceSet: vi.fn(),
	scrobbleReset: vi.fn()
}));

const storage = new Map<string, string>();
vi.stubGlobal('localStorage', {
	getItem: (key: string) => storage.get(key) ?? null,
	setItem: (key: string, value: string) => storage.set(key, value),
	removeItem: (key: string) => storage.delete(key)
});

vi.mock('$app/environment', () => ({ browser: true }));
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
vi.mock('$lib/queries/QueryClient', () => ({
	queryClient: { ensureQueryData: state.ensureQueryData },
	resetQueryCacheForUserSwitch: state.resetQueryCache
}));
vi.mock('$lib/queries/scrobble-preferences/ScrobblePreferencesQuery.svelte', () => ({
	getScrobblePreferencesQueryOptions: (userId: string | undefined) => ({
		queryKey: ['me', 'scrobble-preferences', userId]
	})
}));
vi.mock('$lib/stores/musicSource', () => ({
	DEFAULT_SOURCE: 'listenbrainz',
	isMusicSource: (value: unknown) => value === 'listenbrainz' || value === 'lastfm',
	musicSourceStore: { reset: state.musicSourceReset, setSource: state.musicSourceSet }
}));
vi.mock('$lib/stores/scrobble.svelte', () => ({
	scrobbleManager: { reset: state.scrobbleReset }
}));
vi.mock('$lib/utils/userSessionCleanup', () => ({
	clearUserSessionState: state.sessionCleanup
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
		get setupRequired() {
			return state.setupRequired;
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
		},
		setSetupRequired(required: boolean) {
			state.setupRequired = required;
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

function loadPage(path = '/') {
	return load({ url: new URL(path, 'http://localhost') } as Parameters<typeof load>[0]);
}

describe('+layout load session bootstrap', () => {
	beforeEach(() => {
		state.apiGet.mockReset();
		state.clear.mockReset();
		state.markInitialized.mockReset();
		state.ensureQueryData.mockReset();
		state.ensureQueryData.mockResolvedValue({ primary_music_source: 'invalid' });
		state.resetQueryCache.mockClear();
		state.sessionCleanup.mockReset();
		state.sessionCleanup.mockImplementation(async (options?: { clearAuth?: boolean }) => {
			state.resetQueryCache();
			state.musicSourceReset();
			state.scrobbleReset();
			storage.delete('test:last-user');
			if (options?.clearAuth !== false) {
				state.clear();
				state.user = null;
			}
		});
		state.musicSourceReset.mockClear();
		state.musicSourceSet.mockClear();
		state.scrobbleReset.mockClear();
		storage.clear();
		state.user = null;
		state.initialized = false;
		state.setupRequired = false;
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

	it('clears every session leg and redirects after persistent cleanup fails on 401', async () => {
		storage.set('test:last-user', 'user-old');
		state.user = user;
		state.sessionCleanup.mockImplementationOnce(async () => {
			state.resetQueryCache();
			state.musicSourceReset();
			state.scrobbleReset();
			storage.delete('test:last-user');
			state.clear();
			state.user = null;
			throw new Error('IndexedDB unavailable');
		});
		state.apiGet
			.mockResolvedValueOnce({ required: false })
			.mockRejectedValueOnce(new ApiError(401, 'Unauthorized'));

		await expect(loadPage()).rejects.toMatchObject({ status: 302, location: '/login' });
		expect(state.sessionCleanup).toHaveBeenCalledOnce();
		expect(state.resetQueryCache).toHaveBeenCalledOnce();
		expect(state.musicSourceReset).toHaveBeenCalledOnce();
		expect(state.scrobbleReset).toHaveBeenCalledOnce();
		expect(state.clear).toHaveBeenCalledOnce();
		expect(storage.has('test:last-user')).toBe(false);
		expect(state.markInitialized).toHaveBeenCalledOnce();
		expect(state.user).toBeNull();
	});

	it('bounds the optional preferences request without discarding the session', async () => {
		state.user = user;
		state.initialized = true;
		state.ensureQueryData.mockRejectedValueOnce(new DOMException('Timed out', 'TimeoutError'));

		await expect(loadPage()).resolves.toEqual({ primarySource: 'listenbrainz', user });
		expect(state.ensureQueryData).toHaveBeenNthCalledWith(1, {
			queryKey: ['me', 'scrobble-preferences', 'user-1']
		});
		expect(state.clear).not.toHaveBeenCalled();
		expect(state.user).toBe(user);
	});

	it('hydrates the primary source through the user-scoped session query', async () => {
		state.user = user;
		state.initialized = true;
		state.ensureQueryData.mockResolvedValueOnce({ primary_music_source: 'lastfm' });

		await expect(loadPage('/library')).resolves.toEqual({ primarySource: 'lastfm', user });
		expect(state.ensureQueryData).toHaveBeenCalledWith({
			queryKey: ['me', 'scrobble-preferences', 'user-1']
		});
		expect(state.apiGet).not.toHaveBeenCalled();
	});

	it('hydrates the MusicBrainz source from the authenticated session response', async () => {
		const source = {
			source_mode: 'mirror',
			source_id: 'mirror-a',
			generation: 7
		};
		state.apiGet
			.mockResolvedValueOnce({ required: false })
			.mockResolvedValueOnce({ ...user, musicbrainz_source: source });
		state.user = null;
		state.initialized = false;

		await expect(loadPage('/library')).resolves.toMatchObject({
			user: { musicbrainz_source: source }
		});
		expect(state.apiGet).toHaveBeenCalledTimes(2);
		expect(state.apiGet).toHaveBeenNthCalledWith(2, '/me', { timeoutMs: 10_000 });
		expect(state.ensureQueryData).toHaveBeenCalledOnce();
	});

	it('keeps the new account active when persistent cleanup fails during user switch', async () => {
		storage.set('test:last-user', 'user-old');
		state.user = user;
		state.initialized = true;
		state.sessionCleanup.mockImplementationOnce(async (options?: { clearAuth?: boolean }) => {
			state.resetQueryCache();
			state.musicSourceReset();
			state.scrobbleReset();
			storage.delete('test:last-user');
			if (options?.clearAuth !== false) {
				state.clear();
				state.user = null;
			}
			throw new Error('IndexedDB unavailable');
		});
		state.ensureQueryData.mockResolvedValueOnce({ primary_music_source: 'lastfm' });

		await expect(loadPage('/')).resolves.toEqual({ primarySource: 'lastfm', user });
		expect(state.sessionCleanup).toHaveBeenCalledWith({ clearAuth: false });
		expect(state.resetQueryCache).toHaveBeenCalledOnce();
		expect(state.musicSourceReset).toHaveBeenCalledOnce();
		expect(state.scrobbleReset).toHaveBeenCalledOnce();
		expect(state.clear).not.toHaveBeenCalled();
		expect(state.musicSourceSet).toHaveBeenCalledWith('lastfm');
		expect(storage.get('test:last-user')).toBe('user-1');
	});

	it('runs setup and session hydration only once across in-app navigation', async () => {
		state.apiGet.mockResolvedValueOnce({ required: false }).mockResolvedValueOnce(user);

		await expect(loadPage()).resolves.toEqual({ primarySource: 'listenbrainz', user });
		await expect(loadPage()).resolves.toEqual({ primarySource: 'listenbrainz', user });

		expect(state.apiGet.mock.calls.filter(([url]) => url === '/setup-status')).toHaveLength(1);
		expect(state.apiGet.mock.calls.filter(([url]) => url === '/me')).toHaveLength(1);
		expect(state.apiGet.mock.calls.filter(([url]) => url === '/scrobble-preferences')).toHaveLength(
			0
		);
		expect(state.ensureQueryData).toHaveBeenCalledTimes(2);
	});

	it('retains setup state across navigation without repeating bootstrap requests', async () => {
		state.apiGet
			.mockResolvedValueOnce({ required: true })
			.mockRejectedValueOnce(new ApiError(401, 'Unauthorized'));

		await expect(loadPage('/login')).resolves.toEqual({
			primarySource: 'listenbrainz',
			user: null
		});
		await expect(loadPage()).rejects.toMatchObject({ status: 302, location: '/setup' });

		expect(state.apiGet.mock.calls.filter(([url]) => url === '/setup-status')).toHaveLength(1);
		expect(state.apiGet.mock.calls.filter(([url]) => url === '/me')).toHaveLength(1);
	});

	it('redirects a configured setup route to login when signed out', async () => {
		state.initialized = true;

		await expect(loadPage('/setup')).rejects.toMatchObject({ status: 302, location: '/login' });
		expect(state.apiGet).not.toHaveBeenCalled();
		expect(state.ensureQueryData).not.toHaveBeenCalled();
	});

	it('redirects a configured setup route home when already authenticated', async () => {
		state.initialized = true;
		state.user = user;

		await expect(loadPage('/setup')).rejects.toMatchObject({ status: 302, location: '/' });
		expect(state.apiGet).not.toHaveBeenCalled();
		expect(state.ensureQueryData).not.toHaveBeenCalled();
	});
});
