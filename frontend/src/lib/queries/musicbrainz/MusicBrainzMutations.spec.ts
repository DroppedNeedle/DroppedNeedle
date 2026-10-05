import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { post: vi.fn(), put: vi.fn() } }
}));

vi.mock('$app/environment', () => ({ browser: true }));

const cache = vi.hoisted(() => ({ set: vi.fn().mockResolvedValue(undefined) }));
vi.mock('$lib/queries/QueryClient', () => ({
	setQueryDataWithPersister: cache.set
}));

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { authStore, type AuthUser } from '$lib/stores/authStore.svelte';
import { saveMusicBrainzSettings } from './MusicBrainzMutations.svelte';
import { getMusicBrainzSourceScope, resetMusicBrainzSourceScope } from './sourceScope.svelte';
import type { MusicBrainzSettingsResponse, MusicBrainzSettingsUpdate } from './types';

const mockPost = vi.mocked(api.global.post);
const mockPut = vi.mocked(api.global.put);

const storage = new Map<string, string>();

function user(id: string): AuthUser {
	return {
		id,
		display_name: id,
		role: 'admin',
		email: `${id}@example.test`,
		avatar_url: null,
		username: id,
		username_display: id,
		providers: []
	};
}

function persistScope(
	userId: string,
	sourceMode: string,
	sourceId: string,
	generation: number
): void {
	storage.set(
		`droppedneedle:musicbrainz-source:${encodeURIComponent(userId)}`,
		JSON.stringify({ sourceMode, sourceId, generation })
	);
}

const scopeStorageKey = (userId: string) =>
	`droppedneedle:musicbrainz-source:${encodeURIComponent(userId)}`;

type MutationOptions = {
	mutationFn: (vars?: unknown) => Promise<unknown>;
	onMutate?: (vars: unknown, context: unknown) => unknown;
	onSuccess?: (data: unknown, vars: unknown, onMutateResult: unknown) => unknown;
};

const settings: MusicBrainzSettingsResponse = {
	source_mode: 'official',
	api_url: 'https://musicbrainz.org/ws/2',
	rate_limit: 1,
	concurrent_searches: 1,
	community_acknowledged: null,
	selected_source_mode: 'brainzmash',
	source_id: 'official-source',
	generation: 1,
	pending_brainzmash: null
};

const update: MusicBrainzSettingsUpdate = {
	source_mode: 'brainzmash',
	api_url: null,
	rate_limit: 1,
	concurrent_searches: 1,
	community_acknowledged: null
};

beforeEach(() => {
	vi.clearAllMocks();
	storage.clear();
	Object.defineProperty(globalThis, 'localStorage', {
		configurable: true,
		value: {
			getItem: (key: string) => storage.get(key) ?? null,
			setItem: (key: string, value: string) => storage.set(key, value),
			removeItem: (key: string) => storage.delete(key)
		}
	});
	authStore.clear();
	resetMusicBrainzSourceScope();
	mockPost.mockResolvedValue(settings);
	mockPut.mockResolvedValue(settings);
});

afterEach(() => {
	authStore.clear();
});

describe('MusicBrainz settings mutations', () => {
	it('does not publish user A response after authentication switches to user B', async () => {
		persistScope('user-b', 'community', 'community-b', 9);
		let resolveResponse: (data: MusicBrainzSettingsResponse) => void = () => {};
		const responsePromise = new Promise<MusicBrainzSettingsResponse>((resolve) => {
			resolveResponse = resolve;
		});
		mockPut.mockReturnValueOnce(responsePromise);

		authStore.setUser(user('user-a'));
		const options = saveMusicBrainzSettings() as unknown as MutationOptions;
		const userAContext = options.onMutate?.(update, {});
		const request = options.mutationFn(update);

		authStore.setUser(user('user-b'));
		resolveResponse(settings);
		const result = await request;
		await options.onSuccess?.(result, update, userAContext);

		expect(mockPut).toHaveBeenCalledWith(API.settingsMusicbrainz(), update);
		expect(getMusicBrainzSourceScope()).toEqual({
			userId: 'user-b',
			sourceMode: 'community',
			sourceId: 'community-b',
			generation: 9
		});
		expect(storage.get(scopeStorageKey('user-b'))).toBe(
			JSON.stringify({ sourceMode: 'community', sourceId: 'community-b', generation: 9 })
		);
		expect(cache.set).not.toHaveBeenCalled();
	});

	it('keeps concurrent mutation responses bound to their initiating users', async () => {
		persistScope('user-b', 'community', 'community-b', 9);
		const options = saveMusicBrainzSettings() as unknown as MutationOptions;

		authStore.setUser(user('user-a'));
		const userAContext = options.onMutate?.(update, {});
		authStore.setUser(user('user-b'));
		const userBContext = options.onMutate?.(update, {});

		await options.onSuccess?.(settings, update, userAContext);
		const userBSettings = { ...settings, source_id: 'source-b', generation: 2 };
		await options.onSuccess?.(userBSettings, update, userBContext);

		expect(cache.set).toHaveBeenCalledOnce();
		expect(getMusicBrainzSourceScope()).toEqual({
			userId: 'user-b',
			sourceMode: 'official',
			sourceId: 'source-b',
			generation: 2
		});
	});

	it('publishes and persists a mutation response when the initiating user remains active', async () => {
		authStore.setUser(user('user-a'));
		const options = saveMusicBrainzSettings() as unknown as MutationOptions;
		const userAContext = options.onMutate?.(update, {});

		await options.onSuccess?.(settings, update, userAContext);

		expect(getMusicBrainzSourceScope()).toEqual({
			userId: 'user-a',
			sourceMode: 'official',
			sourceId: 'official-source',
			generation: 1
		});
		expect(storage.get(scopeStorageKey('user-a'))).toBe(
			JSON.stringify({ sourceMode: 'official', sourceId: 'official-source', generation: 1 })
		);
		expect(cache.set).toHaveBeenCalledOnce();
	});

	it('keeps the committed source in memory when persisting the response fails', async () => {
		authStore.setUser(user('user-a'));
		const options = saveMusicBrainzSettings() as unknown as MutationOptions;
		const onMutateResult = options.onMutate?.(update, {});
		cache.set.mockRejectedValueOnce(new Error('IndexedDB unavailable'));

		await expect(options.onSuccess?.(settings, update, onMutateResult)).resolves.toBeUndefined();
		expect(getMusicBrainzSourceScope()).toEqual({
			userId: 'user-a',
			sourceMode: 'official',
			sourceId: 'official-source',
			generation: 1
		});
		expect(cache.set).toHaveBeenCalledOnce();
	});
});
