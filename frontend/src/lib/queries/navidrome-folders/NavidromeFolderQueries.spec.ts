import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { get: vi.fn(), put: vi.fn() } }
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('$lib/utils/navidromeLibraryCache', () => ({
	clearNavidromeLocalCaches: vi.fn(),
	setNavidromeFolderScopeRevision: vi.fn()
}));

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { NavidromeFolderQueryKeyFactory } from './NavidromeFolderQueryKeyFactory';
import { createUpdateNavidromeFolderPreferenceMutation } from './NavidromeFolderMutations.svelte';
import { getNavidromeFolderPreferenceQuery } from './NavidromeFolderQueries.svelte';

type Options = {
	queryKey?: unknown;
	queryFn?: (context: { signal: AbortSignal }) => Promise<unknown>;
	mutationFn?: (body: unknown) => Promise<unknown>;
};

beforeEach(() => vi.clearAllMocks());

describe('Navidrome folder preference queries', () => {
	it('keys preferences by user and catalogs by user plus scope', () => {
		expect(NavidromeFolderQueryKeyFactory.preferences('alice')).toEqual([
			'navidrome',
			'folder-preferences',
			'alice'
		]);
		expect(NavidromeFolderQueryKeyFactory.catalog('alice', 'selected-a')).toEqual([
			'navidrome',
			'catalog',
			'alice',
			'selected-a'
		]);
		expect(NavidromeFolderQueryKeyFactory.catalog('bob', 'selected-a')).not.toEqual(
			NavidromeFolderQueryKeyFactory.catalog('alice', 'selected-a')
		);
	});

	it('fetches the authenticated preference route with an abort signal', async () => {
		vi.mocked(api.global.get).mockResolvedValue({});
		const options = getNavidromeFolderPreferenceQuery(() => 'alice') as unknown as Options;
		const signal = new AbortController().signal;
		await options.queryFn!({ signal });
		expect(api.global.get).toHaveBeenCalledWith(API.me.navidromeMusicFolderPreferences(), {
			signal
		});
	});

	it('saves the complete selection in one request', async () => {
		vi.mocked(api.global.put).mockResolvedValue({});
		const options = createUpdateNavidromeFolderPreferenceMutation(
			() => 'alice'
		) as unknown as Options;
		const body = { mode: 'selected', selected_folder_ids: ['a', 'b'] };
		await options.mutationFn!(body);
		expect(api.global.put).toHaveBeenCalledWith(API.me.navidromeMusicFolderPreferences(), body);
	});
});
