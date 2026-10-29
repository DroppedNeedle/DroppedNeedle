import { page } from 'vitest/browser';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { PlaylistDetailV3 } from '$lib/queries/playlists/PlaylistV3Queries.svelte';
import { PlaylistQueryKeyFactory } from '$lib/queries/playlists/PlaylistQueryKeyFactory';

const mockShareMutate = vi.fn();
const mockDeleteMutate = vi.fn();
const mockResolveMutate = vi.fn();
const mockRenameMutate = vi.fn();
const mockInvalidate = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));

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

vi.mock('$lib/queries/playlists/PlaylistV3Queries.svelte', () => ({
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

vi.mock('$lib/queries/playlists/PlaylistV3Mutations.svelte', () => ({
	setPlaylistVisibilityV3: () => ({ mutateAsync: mockShareMutate, isPending: false }),
	deletePlaylistV3: () => ({ mutateAsync: mockDeleteMutate, isPending: false }),
	updatePlaylistV3: () => ({ mutateAsync: mockRenameMutate, isPending: false }),
	uploadPlaylistCoverV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	deletePlaylistCoverV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	removePlaylistTrackV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	removePlaylistTracksV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	updatePlaylistTrackV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	reorderPlaylistTrackV3: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

// A real client with no persister; the invalidation helper is a spy.
vi.mock('$lib/queries/QueryClient', async () => {
	const { QueryClient } = await import('@tanstack/svelte-query');
	const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return {
		queryClient,
		invalidateQueriesWithPersister: mockInvalidate,
		setQueryDataWithPersister: async (key: readonly unknown[], data: unknown): Promise<void> => {
			// eslint-disable-next-line no-restricted-syntax -- this stands in for the helper itself
			queryClient.setQueryData(key, data);
		}
	};
});

// The discovery modal (inside the header) reads V3 suggestions.
vi.mock('$lib/queries/discover/DiscoverV3Queries.svelte', () => ({
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
vi.mock('$lib/stores/player.svelte', () => ({
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

	it('owner sees the share toggle', async () => {
		detailQuery.data = makePlaylist({ is_owner: true });
		await renderDetail('pl-1');

		await expect
			.element(page.getByRole('checkbox', { name: /Make playlist public/ }))
			.toBeVisible();
	});

	it('non-owner public view is read-only (no share toggle, no edit name)', async () => {
		detailQuery.data = makePlaylist({ is_owner: false, is_public: true, owner_name: 'Ann' });
		await renderDetail('pl-1');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();
		await expect.element(page.getByText(/Shared by Ann/)).toBeVisible();
		expect(page.getByRole('button', { name: /Edit playlist name/ }).elements()).toHaveLength(0);
		expect(
			page.getByRole('checkbox', { name: /Make playlist (public|private)/ }).elements()
		).toHaveLength(0);
	});

	it('missing banner counts albums without sources but skips library_file_id rows', async () => {
		detailQuery.data = makePlaylist({
			tracks: [
				makeTrack({
					id: 'trk-missing',
					track_name: 'Missing Track',
					album_id: 'alb-missing',
					available_sources: [],
					library_file_id: null,
					track_source_id: null
				}),
				makeTrack({
					id: 'trk-owned',
					track_name: 'Owned Track',
					album_id: 'alb-owned',
					available_sources: [],
					library_file_id: '42',
					track_source_id: null
				})
			],
			track_count: 2
		});
		await renderDetail('pl-1');

		await expect.element(page.getByText('Missing Track')).toBeVisible();
		// Owned library_file_id rows are skipped, so only 1 album is missing.
		await expect.element(page.getByText(/not in your library/)).toBeVisible();
		expect(page.getByText(/albums not in your library/).elements()).toHaveLength(0);
		await expect
			.element(page.getByRole('button', { name: 'Request album', exact: true }))
			.toBeVisible();
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
			expect(mockInvalidate).toHaveBeenCalledWith(
				{ queryKey: PlaylistQueryKeyFactory.sources(undefined, 'pl-1'), exact: true },
				undefined,
				{ removePersisted: true }
			);
		});
	});
});
