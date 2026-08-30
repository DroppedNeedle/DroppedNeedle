import { beforeEach, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({
	localCaches: vi.fn(),
	memory: vi.fn(),
	persisted: vi.fn(),
	auth: vi.fn(),
	music: vi.fn(),
	scrobble: vi.fn()
}));

vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$lib/queries/IndexedDbPersister.svelte', () => ({
	clearPersistedQueryCache: state.persisted
}));
vi.mock('$lib/queries/QueryClient', () => ({
	queryClient: { clear: state.memory }
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: { clear: state.auth }
}));
vi.mock('$lib/utils/userScopedCaches', () => ({
	clearUserScopedLocalCaches: state.localCaches
}));

import { clearUserSessionState, registerUserSessionReset } from './userSessionCleanup';

const storage = new Map<string, string>();
vi.stubGlobal('localStorage', {
	removeItem: (key: string) => storage.delete(key)
});

beforeEach(() => {
	state.localCaches.mockReset();
	state.memory.mockReset();
	state.persisted.mockReset();
	state.persisted.mockResolvedValue(undefined);
	state.auth.mockReset();
	state.music.mockReset();
	state.scrobble.mockReset();
	storage.clear();
	storage.set('test:last-user', 'user-old');
});

it('attempts every synchronous leg before rethrowing persistent cleanup failure', async () => {
	const unregisterMusic = registerUserSessionReset(state.music);
	const unregisterScrobble = registerUserSessionReset(state.scrobble);
	const persistedFailure = new Error('IndexedDB unavailable');
	state.persisted.mockRejectedValueOnce(persistedFailure);

	await expect(clearUserSessionState()).rejects.toBe(persistedFailure);

	expect(state.localCaches).toHaveBeenCalledOnce();
	expect(state.music).toHaveBeenCalledOnce();
	expect(state.scrobble).toHaveBeenCalledOnce();
	expect(state.memory).toHaveBeenCalledOnce();
	expect(state.auth).toHaveBeenCalledOnce();
	expect(storage.has('test:last-user')).toBe(false);
	expect(state.persisted).toHaveBeenCalledOnce();

	unregisterMusic();
	unregisterScrobble();
});
