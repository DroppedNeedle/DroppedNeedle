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
	scrobbleReset: vi.fn(),
	shellLoad: vi.fn()
}));

const storage = new Map<string, string>();
vi.stubGlobal('localStorage', {
	getItem: (key: string) => storage.get(key) ?? null,
	setItem: (key: string, value: string) => storage.set(key, value),
	removeItem: (key: string) => storage.delete(key)
});

vi.mock('$app/environment', () => ({ browser: true }));
// imported for its side effect (applying the theme), which needs a DOM
vi.mock('$lib/stores/theme.svelte', () => ({}));
vi.mock('$lib/api/client', () => ({ api: { v3: { GET: state.apiGet } } }));
vi.mock('$lib/constants', () => ({
	AUTH_FREE_PATHS: ['/login', '/setup', '/upgrade'],
	SHELL_FREE_PATHS: ['/login', '/setup', '/upgrade', '/connect'],
	UPGRADE_PATH: '/upgrade'
}));
vi.mock('$lib/shell/lazyComponentLoaders', () => ({
	loadAuthenticatedAppShell: state.shellLoad
}));
vi.mock('$lib/queries/QueryClient', () => ({
	queryClient: {
		ensureQueryData: state.ensureQueryData,
		getQueryData: () => undefined,
		prefetchQuery: vi.fn()
	},
	resetQueryCacheForUserSwitch: state.resetQueryCache,
	collectPersistedGarbage: vi.fn()
}));
vi.mock('$lib/features/profile/queries/scrobble-preferences.svelte', () => ({
	getScrobblePreferencesQueryOptions: (userId: string | undefined) => ({ userId })
}));
vi.mock('$lib/stores/musicSource', () => ({
	DEFAULT_SOURCE: 'listenbrainz',
	isMusicSource: (value: unknown) => value === 'listenbrainz' || value === 'lastfm'
}));
vi.mock('$lib/features/player/state/scrobble.svelte', () => ({
	scrobbleManager: { reset: state.scrobbleReset }
}));
vi.mock('$lib/utils/userSessionCleanup', () => ({
	clearUserSessionState: state.sessionCleanup
}));
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

import { ApiError, TransportError } from '$lib/api/errors';
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

function reset(): void {
	state.apiGet.mockReset();
	state.apiGet.mockRejectedValue(new Error('no answer set for this request'));
	state.shellLoad.mockReset();
	state.shellLoad.mockResolvedValue(() => null);
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
	state.scrobbleReset.mockClear();
	storage.clear();
	state.user = null;
	state.initialized = false;
	state.setupRequired = false;
}

describe('+layout load session bootstrap', () => {
	beforeEach(reset);

	it('setup status and the session read both start before either answers', async () => {
		storage.set('test:last-user', 'user-1');
		const answers: Array<(value: unknown) => void> = [];
		state.apiGet.mockImplementation(() => new Promise((resolve) => answers.push(resolve)));
		state.ensureQueryData.mockImplementationOnce((options: { queryFn: () => unknown }) =>
			options.queryFn()
		);

		const loading = loadPage();

		// A session is likely, so its preferences and the shell's code start too.
		expect(state.apiGet.mock.calls.map(([url]) => url)).toEqual([
			'/api/v3/auth/setup/status',
			'/api/v3/me',
			'/api/v3/me/scrobble-preferences'
		]);
		expect(state.shellLoad).toHaveBeenCalledOnce();
		answers[0]({ setup_required: false, code_required: false });
		answers[1](user);
		answers[2]({ primary_music_source: 'lastfm' });
		await expect(loading).resolves.toEqual({ primarySource: 'lastfm', user });
		expect(state.apiGet).toHaveBeenCalledTimes(3);
		expect(state.shellLoad).toHaveBeenCalledOnce();
	});

	it('starts the shell once the session answers when none was likely, and never on sign-in', async () => {
		state.apiGet
			.mockResolvedValueOnce({ setup_required: false, code_required: false })
			.mockRejectedValueOnce(new ApiError(401, 'Unauthorized'));
		await expect(loadPage('/login')).resolves.toMatchObject({ user: null });
		expect(state.shellLoad).not.toHaveBeenCalled();

		reset();
		state.apiGet
			.mockResolvedValueOnce({ setup_required: false, code_required: false })
			.mockResolvedValueOnce(user);
		await expect(loadPage('/library')).resolves.toMatchObject({ user });
		expect(state.shellLoad).toHaveBeenCalledOnce();
		expect(state.apiGet).toHaveBeenCalledTimes(2);
	});

	it('keeps the session intact and reports an unreachable server when /api/v3/me times out', async () => {
		state.user = user;
		state.apiGet
			.mockResolvedValueOnce({ setup_required: false, code_required: false })
			.mockRejectedValueOnce(new TransportError('TRANSPORT_TIMEOUT', 'GET', '/api/v3/me'));

		await expect(loadPage()).rejects.toMatchObject({
			status: 503,
			body: { kind: 'unreachable' }
		});
		expect(state.clear).not.toHaveBeenCalled();
		expect(state.user).toBe(user);
		expect(state.apiGet).toHaveBeenNthCalledWith(1, '/api/v3/auth/setup/status', {
			timeoutMs: 10_000
		});
		expect(state.apiGet).toHaveBeenNthCalledWith(2, '/api/v3/me', { timeoutMs: 10_000 });
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
			.mockResolvedValueOnce({ setup_required: false, code_required: false })
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
		expect(state.ensureQueryData).toHaveBeenNthCalledWith(1, { userId: 'user-1' });
		expect(state.clear).not.toHaveBeenCalled();
		expect(state.user).toBe(user);
	});

	it('hydrates the primary source through the user-scoped session query', async () => {
		state.user = user;
		state.initialized = true;
		state.ensureQueryData.mockResolvedValueOnce({ primary_music_source: 'lastfm' });

		await expect(loadPage('/library')).resolves.toEqual({ primarySource: 'lastfm', user });
		expect(state.ensureQueryData).toHaveBeenCalledWith({ userId: 'user-1' });
		expect(state.apiGet).not.toHaveBeenCalled();
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
		expect(storage.get('test:last-user')).toBe('user-1');
	});

	it('runs the setup check and session bootstrap once across in-app navigation', async () => {
		{
			// Runs setup and session hydration only once across in-app navigation.
			reset();
			state.apiGet
				.mockResolvedValueOnce({ setup_required: false, code_required: false })
				.mockResolvedValueOnce(user);

			await expect(loadPage()).resolves.toEqual({ primarySource: 'listenbrainz', user });
			await expect(loadPage()).resolves.toEqual({ primarySource: 'listenbrainz', user });

			expect(
				state.apiGet.mock.calls.filter(([url]) => url === '/api/v3/auth/setup/status')
			).toHaveLength(1);
			expect(state.apiGet.mock.calls.filter(([url]) => url === '/api/v3/me')).toHaveLength(1);
			expect(
				state.apiGet.mock.calls.filter(([url]) => url === '/scrobble-preferences')
			).toHaveLength(0);
			expect(state.ensureQueryData).toHaveBeenCalledTimes(2);
		}

		{
			// Retains setup state across navigation without repeating bootstrap requests.
			reset();
			state.apiGet
				.mockResolvedValueOnce({ setup_required: true, code_required: false })
				.mockRejectedValueOnce(new ApiError(401, 'Unauthorized'));

			await expect(loadPage('/login')).resolves.toEqual({
				primarySource: 'listenbrainz',
				user: null
			});
			await expect(loadPage()).rejects.toMatchObject({ status: 302, location: '/setup' });

			expect(
				state.apiGet.mock.calls.filter(([url]) => url === '/api/v3/auth/setup/status')
			).toHaveLength(1);
			expect(state.apiGet.mock.calls.filter(([url]) => url === '/api/v3/me')).toHaveLength(1);
		}
	});

	it('sends a configured setup route to sign-in, or home when signed in, with no requests', async () => {
		for (const [signedIn, location] of [
			[null, '/login'],
			[user, '/']
		] as const) {
			reset();
			state.initialized = true;
			state.user = signedIn;

			await expect(loadPage('/setup')).rejects.toMatchObject({ status: 302, location });
			expect(state.apiGet).not.toHaveBeenCalled();
			expect(state.ensureQueryData).not.toHaveBeenCalled();
		}
	});

	it('sends a server started on v2 data to the upgrade page, which loads without the bootstrap', async () => {
		state.apiGet.mockRejectedValueOnce(
			new ApiError(503, 'An upgrade is needed', 'UPGRADE_REQUIRED')
		);

		await expect(loadPage('/library')).rejects.toMatchObject({
			status: 302,
			location: '/upgrade'
		});
		state.apiGet.mockReset();
		await expect(loadPage('/upgrade')).resolves.toMatchObject({ user: null });
		expect(state.apiGet).not.toHaveBeenCalled();
	});
});
