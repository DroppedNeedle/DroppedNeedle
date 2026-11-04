import { v3 } from '$lib/api/v3/endpoint';
import { describe, expect, it, vi, beforeEach } from 'vitest';

vi.mock('$lib/queries/effects', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/effects')>()),
	createAppMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { PUT: vi.fn(), DELETE: vi.fn() } }
}));

const { mockRemoveMbid } = vi.hoisted(() => ({ mockRemoveMbid: vi.fn() }));

vi.mock('$lib/stores/library', () => ({
	libraryStore: { removeMbid: mockRemoveMbid }
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined),
	setQueryDataWithPersister: vi.fn().mockResolvedValue(undefined)
}));

import { api } from '$lib/api/client';
import { createRemoveLibraryAlbumMutation } from './LibraryMutations.svelte';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import type { Effect } from '$lib/queries/effects';

// The typed client's DELETE resolves to the response body; the mock stands in for it.
const mockDelete = vi.mocked(api.v3.DELETE) as unknown as ReturnType<typeof vi.fn>;

beforeEach(() => {
	vi.clearAllMocks();
	mockDelete.mockResolvedValue({});
});

type RemoveInput = {
	mbid: string;
	albumId: string;
	expectedRevision: number;
	expectedListingRevision: number;
	stopWanted: boolean;
};

describe('album removal mutation', () => {
	it('clears exact membership and declares the catalog and wanted changed', async () => {
		const result = { id: 'rg-1', removed_track_ids: [] };
		mockDelete.mockResolvedValueOnce(result);
		const mutation = createRemoveLibraryAlbumMutation() as unknown as {
			mutationFn: (input: RemoveInput) => Promise<typeof result>;
			onSuccess: (data: typeof result, input: RemoveInput) => Promise<void>;
			effects: () => Effect[];
		};

		const input = {
			mbid: 'release-1',
			albumId: 'local-1',
			expectedRevision: 4,
			expectedListingRevision: 6,
			stopWanted: true
		};
		const data = await mutation.mutationFn(input);
		await mutation.onSuccess(data, input);

		expect(mockDelete).toHaveBeenCalledWith(
			v3('/api/v3/library/albums/{id}', {
				path: { id: 'local-1' },
				query: {
					expected_revision: 4,
					expected_listing_revision: 6,
					delete_files: true,
					stop_wanted: true
				}
			})
		);
		expect(mockRemoveMbid).toHaveBeenCalledWith('release-1');
		expect(mockRemoveMbid).toHaveBeenCalledWith('rg-1');
		expect(setQueryDataWithPersister).toHaveBeenCalledWith(
			LibraryQueryKeyFactory.album(undefined, 'release-1'),
			expect.any(Function)
		);
		expect(mutation.effects().map((effect) => effect.name)).toEqual([
			'catalogChanged',
			'anyWantedChanged'
		]);
	});

	it('does not report cache housekeeping failures as removal failures', async () => {
		const result = { id: 'rg-1', removed_track_ids: [] };
		mockDelete.mockResolvedValueOnce(result);
		vi.mocked(setQueryDataWithPersister).mockRejectedValueOnce(new Error('IndexedDB unavailable'));
		const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);
		const mutation = createRemoveLibraryAlbumMutation() as unknown as {
			mutationFn: (input: RemoveInput) => Promise<typeof result>;
			onSuccess: (data: typeof result, input: RemoveInput) => Promise<void>;
		};

		const input = {
			mbid: 'rg-1',
			albumId: 'local-1',
			expectedRevision: 4,
			expectedListingRevision: 6,
			stopWanted: false
		};
		const data = await mutation.mutationFn(input);

		await expect(mutation.onSuccess(data, input)).resolves.toBeUndefined();
		expect(consoleError).toHaveBeenCalledTimes(1);
		consoleError.mockRestore();
	});
});
