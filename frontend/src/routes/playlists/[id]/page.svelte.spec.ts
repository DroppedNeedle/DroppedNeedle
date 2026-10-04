import { page, userEvent } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { PlaylistDetailV3 } from '$lib/queries/playlists/PlaylistV3Queries.svelte';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const mockShareMutate = vi.fn();
const mockDeleteMutate = vi.fn();
const mockResolveMutate = vi.fn();
const mockRenameMutate = vi.fn();

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
	getPlaylistDetailV3Query: () => detailQuery
}));

vi.mock('$lib/queries/playlists/PlaylistV3Mutations.svelte', () => ({
	setPlaylistVisibilityV3: () => ({ mutateAsync: mockShareMutate, isPending: false }),
	deletePlaylistV3: () => ({ mutateAsync: mockDeleteMutate, isPending: false }),
	resolvePlaylistSourcesV3: () => ({ mutateAsync: mockResolveMutate, isPending: false }),
	updatePlaylistV3: () => ({ mutateAsync: mockRenameMutate, isPending: false }),
	uploadPlaylistCoverV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	deletePlaylistCoverV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	removePlaylistTrackV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	removePlaylistTracksV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	updatePlaylistTrackV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	reorderPlaylistTrackV3: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

// PlaylistDiscoveryModal (inside the header) still reads the v1 list query.
vi.mock('$lib/queries/playlists/PlaylistQuery.svelte', () => ({
	getPlaylistListQuery: () => ({
		data: [],
		isLoading: false,
		isError: false,
		error: null,
		refetch: vi.fn()
	})
}));

vi.mock('$lib/queries/QueryClient', () => {
	// musicSource/userSessionCleanup ride along via $lib/constants and import
	// the client value + setter; they only run at call time, so a small
	// in-memory client is enough.
	const store = new Map<string, unknown>();
	const keyOf = (key: unknown) => JSON.stringify(key);
	const fakeClient = {
		getQueryData: (key: unknown) => store.get(keyOf(key)),
		setQueryData: (key: unknown, updater: unknown) => {
			const next =
				typeof updater === 'function'
					? (updater as (old: unknown) => unknown)(store.get(keyOf(key)))
					: updater;
			store.set(keyOf(key), next);
			return next;
		},
		removeQueries: (filters?: { queryKey?: unknown }) => {
			if (filters?.queryKey === undefined) {
				store.clear();
				return;
			}
			const prefix = keyOf(filters.queryKey).slice(0, -1);
			for (const k of [...store.keys()]) {
				if (k.startsWith(prefix)) store.delete(k);
			}
		},
		invalidateQueries: vi.fn(async () => undefined),
		cancelQueries: vi.fn(async () => undefined),
		clear: () => store.clear(),
		ensureQueryData: async (opts: {
			queryKey: unknown;
			queryFn: (ctx: { queryKey: unknown; signal: AbortSignal }) => Promise<unknown>;
		}) => {
			const k = keyOf(opts.queryKey);
			if (!store.has(k)) {
				store.set(
					k,
					await opts.queryFn({
						queryKey: opts.queryKey,
						signal: new AbortController().signal
					})
				);
			}
			return store.get(k);
		}
	};
	return {
		queryClient: fakeClient,
		invalidateQueriesWithPersister: vi.fn(),
		setQueryDataWithPersister: async (key: unknown, updater: unknown): Promise<void> => {
			fakeClient.setQueryData(key, updater);
		}
	};
});

vi.mock('$lib/queries/discover/DiscoverQuery.svelte', () => ({
	getPlaylistSuggestionsQuery: () => ({
		data: undefined,
		isLoading: false,
		isError: false,
		error: null,
		refetch: vi.fn()
	})
}));

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

	it('renders header with playlist name, track count, and duration', async () => {
		detailQuery.data = makePlaylist();
		await renderDetail('pl-1');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();
		await expect.element(page.getByText(/2 tracks/)).toBeVisible();
		await expect.element(page.getByText(/8 min/)).toBeVisible();
	});

	it('renders track rows with correct data', async () => {
		detailQuery.data = makePlaylist();
		await renderDetail('pl-1');

		await expect.element(page.getByText('First Track')).toBeVisible();
		await expect.element(page.getByText('Second Track')).toBeVisible();
		await expect.element(page.getByText('Other Artist')).toBeVisible();
	});

	it('shows the import chip for a V3 detail carrying source_ref', async () => {
		detailQuery.data = makePlaylist({ source_ref: 'plex:xyz' });
		await renderDetail('pl-1');

		await expect.element(page.getByText(/Imported from Plex/)).toBeVisible();
	});

	it('shows error state when playlist is missing', async () => {
		detailQuery.data = undefined;
		detailQuery.isError = true;
		detailQuery.error = new Error('404 not found');
		await renderDetail('pl-bad');

		await expect.element(page.getByText("Couldn't load this playlist")).toBeVisible();
		await expect.element(page.getByText('Playlist not found')).toBeVisible();
	});

	it('shows empty state when playlist has no tracks', async () => {
		detailQuery.data = makePlaylist({ tracks: [], track_count: 0 });
		await renderDetail('pl-1');

		await expect.element(page.getByText('This playlist is empty')).toBeVisible();
	});

	it('Play All calls playQueue with all tracks', async () => {
		detailQuery.data = makePlaylist();
		await renderDetail('pl-1');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();

		await page.getByRole('button', { name: /Play All/ }).click();

		expect(mockPlayQueue).toHaveBeenCalledOnce();
		const [items, startIdx, shuffle] = mockPlayQueue.mock.calls[0];
		expect(items).toHaveLength(2);
		expect(startIdx).toBe(0);
		expect(shuffle).toBe(false);
	});

	it('Shuffle calls playQueue with shuffle=true', async () => {
		detailQuery.data = makePlaylist();
		await renderDetail('pl-1');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();

		await page.getByRole('button', { name: /Shuffle/ }).click();

		expect(mockPlayQueue).toHaveBeenCalledOnce();
		expect(mockPlayQueue.mock.calls[0][2]).toBe(true);
	});

	it('Play All is disabled when playlist has no tracks', async () => {
		detailQuery.data = makePlaylist({ tracks: [], track_count: 0 });
		await renderDetail('pl-1');

		await expect.element(page.getByText('This playlist is empty')).toBeVisible();
		const playBtn = page.getByRole('button', { name: /Play All/ });
		expect(await playBtn.element()).toBeDisabled();
	});

	it('back button is visible when playlist loads', async () => {
		detailQuery.data = makePlaylist();
		await renderDetail('pl-1');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: /Go back/ })).toBeVisible();
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

	it('inline name editing: clicking name shows input, Escape cancels', async () => {
		detailQuery.data = makePlaylist();
		await renderDetail('pl-1');

		await page.getByRole('button', { name: /Edit playlist name/ }).click();
		const nameInput = page.getByPlaceholder('Playlist name');
		await expect.element(nameInput).toBeVisible();

		await userEvent.keyboard('{Escape}');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();
		expect(mockRenameMutate).not.toHaveBeenCalled();
	});

	it('inline name editing: Enter saves new name', async () => {
		mockRenameMutate.mockResolvedValue(makePlaylist({ name: 'Renamed' }));
		detailQuery.data = makePlaylist();
		await renderDetail('pl-1');

		await page.getByRole('button', { name: /Edit playlist name/ }).click();
		const nameInput = page.getByPlaceholder('Playlist name');
		await expect.element(nameInput).toBeVisible();
		await nameInput.clear();
		await nameInput.fill('Renamed');
		await userEvent.keyboard('{Enter}');

		expect(mockRenameMutate).toHaveBeenCalledOnce();
		expect(mockRenameMutate.mock.calls[0][0]).toEqual({ id: 'pl-1', name: 'Renamed' });
	});

	it('calls resolvePlaylistSources after playlist loads', async () => {
		detailQuery.data = makePlaylist();
		mockResolveMutate.mockResolvedValue({ sources: {} });
		await renderDetail('pl-1');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();
		await vi.waitFor(() => {
			expect(mockResolveMutate).toHaveBeenCalledWith('pl-1');
		});
	});

	it('shows play button on track hover with correct aria label', async () => {
		detailQuery.data = makePlaylist();
		await renderDetail('pl-1');

		await expect.element(page.getByText('First Track')).toBeVisible();
		expect(page.getByRole('button', { name: 'Play First Track' }).elements()).toHaveLength(1);
	});

	it('play button on track calls playQueue with correct start index', async () => {
		detailQuery.data = makePlaylist();
		await renderDetail('pl-1');

		await expect.element(page.getByText('Second Track')).toBeVisible();
		await page.getByRole('button', { name: 'Play Second Track' }).click();

		expect(mockPlayQueue).toHaveBeenCalledOnce();
		const [items, startIdx, shuffle] = mockPlayQueue.mock.calls[0];
		expect(items).toHaveLength(2);
		expect(startIdx).toBe(1);
		expect(shuffle).toBe(false);
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
		// Let the resolve promise chain settle, then assert nothing was cached.
		await new Promise((r) => setTimeout(r, 100));
		// Empty resolve maps are never fresh, so no per-user cache entry is stored.
		expect(localStorage.getItem('droppedneedle_playlist_sources_anon_pl-1')).toBeNull();
	});
});
