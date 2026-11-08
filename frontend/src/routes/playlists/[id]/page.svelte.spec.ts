import { page } from 'vitest/browser';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { PlaylistDetailV3 } from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
import { PlaylistQueryKeyFactory } from '$lib/features/playlists/queries/PlaylistQueryKeyFactory';

const mockShareMutate = vi.fn();
const mockDeleteMutate = vi.fn();
const mockResolveMutate = vi.fn();
const mockRenameMutate = vi.fn();
const mockForgetSources = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));

vi.mock('$lib/api/playlists', () => ({
	isRedactedPlaylist: (p: { is_redacted?: boolean } | null | undefined) => p?.is_redacted === true,
	requestMissingTracks: vi.fn()
}));

// The detail page consumes the user-scoped TanStack detail query + V3 mutations;
// stub them so it renders without a QueryClientProvider and tests drive data directly.
const detailQuery = {
	data: undefined as PlaylistDetailV3 | undefined,
	isLoading: false,
	isError: false,
	error: null as Error | null,
	refetch: vi.fn()
};

vi.mock('$lib/features/playlists/queries/PlaylistV3Queries.svelte', () => ({
	getPlaylistDetailV3Query: () => detailQuery,
	// PlaylistDiscoveryModal (inside the header) reads the playlist list.
	getPlaylistListV3Query: () => ({
		data: [],
		isLoading: false,
		isError: false,
		error: null,
		refetch: vi.fn()
	}),
	isRedactedPlaylistV3: () => false,
	getPlaylistSourcesV3QueryOptions: (userId: string | undefined, id: string) => ({
		queryKey: PlaylistQueryKeyFactory.sources(userId, id),
		queryFn: async () => (await mockResolveMutate(id)).sources
	})
}));

vi.mock('$lib/features/playlists/queries/PlaylistV3Mutations.svelte', () => ({
	createSetPlaylistVisibilityV3Mutation: () => ({ mutateAsync: mockShareMutate, isPending: false }),
	createDeletePlaylistV3Mutation: () => ({ mutateAsync: mockDeleteMutate, isPending: false }),
	createUpdatePlaylistV3Mutation: () => ({ mutateAsync: mockRenameMutate, isPending: false }),
	createUploadPlaylistCoverV3Mutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createDeletePlaylistCoverV3Mutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createRemovePlaylistTrackV3Mutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createRemovePlaylistTracksV3Mutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createUpdatePlaylistTrackV3Mutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createReorderPlaylistTrackV3Mutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	refreshPlaylist: vi.fn().mockResolvedValue(undefined),
	forgetPlaylistSources: mockForgetSources
}));

// A real client with no persister.
vi.mock('$lib/queries/QueryClient', async () => {
	const { QueryClient } = await import('@tanstack/svelte-query');
	const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return {
		queryClient,
		invalidateQueriesWithPersister: vi.fn(),
		setQueryDataWithPersister: async (key: readonly unknown[], data: unknown): Promise<void> => {
			queryClient.setQueryData(key, data);
		}
	};
});

// The discovery modal (inside the header) reads V3 suggestions.
vi.mock('$lib/features/discover/queries/DiscoverV3Queries.svelte', () => ({
	getDiscoverPlaylistSuggestionsV3Query: () => ({
		data: undefined,
		isLoading: false,
		isError: false,
		error: null,
		refetch: vi.fn()
	})
}));

const mockToastShow = vi.fn();
vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: (...args: unknown[]) => mockToastShow(...args) }
}));

const mockPlayQueue = vi.fn();
const mockAddToQueue = vi.fn();
const mockPlayNext = vi.fn();
vi.mock('$lib/features/player/state/player.svelte', () => ({
	playerStore: {
		playQueue: (...args: unknown[]) => mockPlayQueue(...args),
		addToQueue: (...args: unknown[]) => mockAddToQueue(...args),
		playNext: (...args: unknown[]) => mockPlayNext(...args)
	}
}));

const mockGoto = vi.fn();
vi.mock('$app/navigation', () => ({
	goto: (...args: unknown[]) => mockGoto(...args)
}));

import DetailPage from './+page.svelte';

async function renderDetail(playlistId = 'pl-1') {
	return await render(DetailPage, {
		props: { data: { playlistId } }
	} as Parameters<typeof render<typeof DetailPage>>[1]);
}

function makeTrack(overrides: Record<string, unknown> = {}): PlaylistDetailV3['tracks'][number] {
	return {
		id: 'trk-1',
		position: 0,
		track_name: 'Test Track',
		artist_name: 'Test Artist',
		album_name: 'Test Album',
		album_id: 'alb-1',
		artist_id: 'art-1',
		track_source_id: 'vid-1',
		cover_url: '/cover.jpg',
		source_type: 'local',
		available_sources: ['local'],
		format: 'flac',
		track_number: 1,
		disc_number: null,
		duration: 240,
		created_at: 1767225600,
		plex_rating_key: null,
		library_file_id: null,
		...overrides
	};
}

function makePlaylist(overrides: Record<string, unknown> = {}): PlaylistDetailV3 {
	return {
		id: 'pl-1',
		name: 'My Playlist',
		track_count: 2,
		total_duration: 480,
		cover_urls: [],
		custom_cover_url: null,
		source_ref: null,
		created_at: 1767225600,
		updated_at: 1767312000,
		is_public: false,
		is_owner: true,
		can_delete: true,
		owner_name: null,
		is_redacted: false,
		tracks: [
			makeTrack({ id: 'trk-1', position: 0, track_name: 'First Track', duration: 240 }),
			makeTrack({
				id: 'trk-2',
				position: 1,
				track_name: 'Second Track',
				artist_name: 'Other Artist',
				duration: 240
			})
		],
		...overrides
	};
}

describe('Playlist detail page', () => {
	beforeEach(() => {
		detailQuery.data = makePlaylist();
		detailQuery.isLoading = false;
		detailQuery.isError = false;
		detailQuery.error = null;
		detailQuery.refetch.mockReset();
		mockShareMutate.mockReset();
		mockDeleteMutate.mockReset();
		mockRenameMutate.mockReset();
		mockResolveMutate.mockReset();
		mockResolveMutate.mockResolvedValue({ sources: {} });
		mockToastShow.mockReset();
		mockPlayQueue.mockReset();
		mockAddToQueue.mockReset();
		mockPlayNext.mockReset();
		mockGoto.mockReset();
		try {
			localStorage.clear();
		} catch {
			// may throw in environments without localStorage
		}
	});

	it('does not cache empty resolve results', async () => {
		detailQuery.data = makePlaylist();
		mockResolveMutate.mockResolvedValue({ sources: {} });
		await renderDetail('pl-1');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();
		await vi.waitFor(() => {
			expect(mockResolveMutate).toHaveBeenCalledWith('pl-1');
		});
		// Empty resolve maps are never fresh, so the kept answer is dropped.
		await vi.waitFor(() => {
			expect(mockForgetSources).toHaveBeenCalledWith('pl-1');
		});
	});

	it('plays the clicked row when an earlier row has no file to play', async () => {
		detailQuery.data = makePlaylist({
			track_count: 4,
			tracks: [
				makeTrack({
					id: 'trk-0',
					position: 0,
					track_name: 'Unplayable Track',
					track_source_id: null,
					available_sources: []
				}),
				makeTrack({ id: 'trk-1', position: 1, track_name: 'Second Track' }),
				makeTrack({ id: 'trk-2', position: 2, track_name: 'Third Track' }),
				makeTrack({ id: 'trk-3', position: 3, track_name: 'Fourth Track' })
			]
		});
		await renderDetail('pl-1');

		await page.getByRole('button', { name: 'Play Third Track' }).click();
		expect(mockPlayQueue).toHaveBeenCalledTimes(1);
		const [items, startIndex] = mockPlayQueue.mock.calls[0];
		expect(items[startIndex]).toMatchObject({ trackName: 'Third Track' });

		await page.getByRole('button', { name: 'Play Unplayable Track' }).click();
		expect(mockPlayQueue).toHaveBeenCalledTimes(1);
		expect(mockToastShow).toHaveBeenCalledWith({ message: 'Track is not playable', type: 'error' });
	});
});
