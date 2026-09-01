import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', async (importOriginal) => {
	const actual = await importOriginal<typeof import('@tanstack/svelte-query')>();
	return {
		...actual,
		createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
	};
});

vi.mock('$lib/api/client', () => ({
	api: { global: { get: vi.fn() } }
}));
vi.mock('$app/environment', () => ({ browser: true }));

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { authStore, type AuthUser } from '$lib/stores/authStore.svelte';
import { getMusicBrainzSettingsQuery } from './MusicBrainzQueries.svelte';
import { MusicBrainzQueryKeyFactory } from './MusicBrainzQueryKeyFactory';
import { getMusicBrainzSourceScope, resetMusicBrainzSourceScope } from './sourceScope.svelte';
import type { MusicBrainzSettingsResponse } from './types';

const mockGet = vi.mocked(api.global.get);

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

type QueryOptions = {
	queryKey: readonly unknown[];
	queryFn: (context: { signal: AbortSignal }) => Promise<MusicBrainzSettingsResponse>;
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
	mockGet.mockResolvedValue({
		source_mode: 'official',
		source_id: 'source-a',
		generation: 1
	} as MusicBrainzSettingsResponse);
});

afterEach(() => {
	authStore.clear();
});
describe('MusicBrainzQueryKeyFactory', () => {
	it('scopes settings by user and source identity', () => {
		const key = MusicBrainzQueryKeyFactory.settings();
		expect(key.slice(0, 2)).toEqual(['musicbrainz', 'settings']);
		expect(key[2]).toEqual(
			expect.objectContaining({
				user_id: null,
				source_mode: 'brainzmash',
				source_id: '',
				generation: 0
			})
		);
	});
});

describe('getMusicBrainzSettingsQuery', () => {
	it('uses the registry endpoint, forwards the abort signal, and updates source scope', async () => {
		const options = getMusicBrainzSettingsQuery() as unknown as QueryOptions;
		const signal = new AbortController().signal;
		expect(options.queryKey).toHaveLength(3);
		expect(options.queryKey.slice(0, 2)).toEqual(['musicbrainz', 'settings']);
		await options.queryFn({ signal });
		expect(MusicBrainzQueryKeyFactory.settings()[2]).toEqual(
			expect.objectContaining({ source_id: 'source-a', generation: 1 })
		);
		expect(mockGet).toHaveBeenCalledWith(API.settingsMusicbrainz(), { signal });
	});
	it('does not publish a response to a different active user', async () => {
		persistScope('user-b', 'community', 'community-b', 9);
		const response = {
			source_mode: 'official',
			source_id: 'source-a',
			generation: 1
		} as MusicBrainzSettingsResponse;
		let resolveResponse: (data: MusicBrainzSettingsResponse) => void = () => {};
		const responsePromise = new Promise<MusicBrainzSettingsResponse>((resolve) => {
			resolveResponse = resolve;
		});
		mockGet.mockReturnValueOnce(responsePromise);

		authStore.setUser(user('user-a'));
		const options = getMusicBrainzSettingsQuery() as unknown as QueryOptions;
		expect(options.queryKey[2]).toEqual(expect.objectContaining({ user_id: 'user-a' }));
		const request = options.queryFn({ signal: new AbortController().signal });

		authStore.setUser(user('user-b'));
		resolveResponse(response);
		await request;

		expect(getMusicBrainzSourceScope()).toEqual({
			userId: 'user-b',
			sourceMode: 'community',
			sourceId: 'community-b',
			generation: 9
		});
		expect(storage.get('droppedneedle:musicbrainz-source:user-b')).toBe(
			JSON.stringify({ sourceMode: 'community', sourceId: 'community-b', generation: 9 })
		);
	});

	it('publishes a response when the initiating user remains active', async () => {
		authStore.setUser(user('user-a'));
		const options = getMusicBrainzSettingsQuery() as unknown as QueryOptions;
		await options.queryFn({ signal: new AbortController().signal });

		expect(getMusicBrainzSourceScope()).toEqual({
			userId: 'user-a',
			sourceMode: 'official',
			sourceId: 'source-a',
			generation: 1
		});
		expect(storage.get('droppedneedle:musicbrainz-source:user-a')).toBe(
			JSON.stringify({ sourceMode: 'official', sourceId: 'source-a', generation: 1 })
		);
	});
});
