import { page, userEvent } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { PlaylistDetail, PlaylistDetailItem, PlaylistTrack } from '$lib/api/playlists';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const mockDeletePlaylist = vi.fn();
const mockAddTracksToPlaylist = vi.fn();
const mockRemoveTrackFromPlaylist = vi.fn();
const mockRemoveTracksFromPlaylist = vi.fn();
const mockUpdatePlaylist = vi.fn();
const mockUpdatePlaylistTrack = vi.fn();
const mockReorderPlaylistTrack = vi.fn();
const mockUploadPlaylistCover = vi.fn();
const mockDeletePlaylistCover = vi.fn();
const mockCheckTrackMembership = vi.fn();
const mockResolvePlaylistSources = vi.fn();

vi.mock('$lib/api/playlists', () => ({
	queueItemToTrackData: (item: unknown) => item,
	isRedactedPlaylist: (p: { is_redacted?: boolean } | null | undefined) => p?.is_redacted === true,
	fetchPlaylist: vi.fn(),
	fetchPlaylists: vi.fn(),
	createPlaylist: vi.fn(),
	deletePlaylist: (...args: unknown[]) => mockDeletePlaylist(...args),
	addTracksToPlaylist: (...args: unknown[]) => mockAddTracksToPlaylist(...args),
	removeTrackFromPlaylist: (...args: unknown[]) => mockRemoveTrackFromPlaylist(...args),
	removeTracksFromPlaylist: (...args: unknown[]) => mockRemoveTracksFromPlaylist(...args),
	updatePlaylist: (...args: unknown[]) => mockUpdatePlaylist(...args),
	updatePlaylistTrack: (...args: unknown[]) => mockUpdatePlaylistTrack(...args),
	reorderPlaylistTrack: (...args: unknown[]) => mockReorderPlaylistTrack(...args),
	uploadPlaylistCover: (...args: unknown[]) => mockUploadPlaylistCover(...args),
	deletePlaylistCover: (...args: unknown[]) => mockDeletePlaylistCover(...args),
	checkTrackMembership: (...args: unknown[]) => mockCheckTrackMembership(...args),
	resolvePlaylistSources: (...args: unknown[]) => mockResolvePlaylistSources(...args),
	requestMissingTracks: vi.fn()
}));

// The detail page consumes the user-scoped TanStack detail query + share mutation;
// stub both so it renders without a QueryClientProvider and tests drive data directly.
const detailQuery = {
	data: undefined as PlaylistDetailItem | undefined,
	isLoading: false,
	isError: false,
	error: null as Error | null,
	refetch: vi.fn()
};

vi.mock('$lib/queries/playlists/PlaylistQuery.svelte', () => ({
	getPlaylistListQuery: () => ({
		data: [],
		isLoading: false,
		isError: false,
		error: null,
		refetch: vi.fn()
	}),
	getPlaylistDetailQuery: () => detailQuery
}));

vi.mock('$lib/queries/playlists/PlaylistMutations.svelte', () => ({
	createSetPlaylistPublicMutation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/QueryClient', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/QueryClient')>()),
	invalidateQueriesWithPersister: vi.fn()
}));

vi.mock('$lib/queries/discover/DiscoverQuery.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/discover/DiscoverQuery.svelte')>()),
	getPlaylistSuggestionsQuery: () => ({
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

vi.mock('$lib/stores/cacheTtl', () => ({
	getCacheTTL: () => 15 * 60 * 1000
}));

import DetailPage from './+page.svelte';

function renderDetail(playlistId = 'pl-1') {
	return render(DetailPage, {
		props: { data: { playlistId } }
	} as Parameters<typeof render<typeof DetailPage>>[1]);
}

function makeTrack(overrides: Partial<PlaylistTrack> = {}): PlaylistTrack {
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
		created_at: '2026-01-01T00:00:00Z',
		plex_rating_key: null,
		...overrides
	};
}

function makePlaylist(overrides: Partial<PlaylistDetail> = {}): PlaylistDetail {
	return {
		id: 'pl-1',
		name: 'My Playlist',
		track_count: 2,
		total_duration: 480,
		cover_urls: [],
		custom_cover_url: null,
		source_ref: null,
		created_at: '2026-01-01T00:00:00Z',
		updated_at: '2026-01-02T00:00:00Z',
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
		mockDeletePlaylist.mockReset();
		mockRemoveTrackFromPlaylist.mockReset();
		mockUpdatePlaylist.mockReset();
		mockUpdatePlaylistTrack.mockReset();
		mockReorderPlaylistTrack.mockReset();
		mockUploadPlaylistCover.mockReset();
		mockDeletePlaylistCover.mockReset();
		mockResolvePlaylistSources.mockReset();
		mockResolvePlaylistSources.mockResolvedValue({});
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
		renderDetail('pl-1');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();
		await expect.element(page.getByText(/2 tracks/)).toBeVisible();
		await expect.element(page.getByText(/8 min/)).toBeVisible();
	});

	it('renders track rows with correct data', async () => {
		detailQuery.data = makePlaylist();
		renderDetail('pl-1');

		await expect.element(page.getByText('First Track')).toBeVisible();
		await expect.element(page.getByText('Second Track')).toBeVisible();
		await expect.element(page.getByText('Other Artist')).toBeVisible();
	});

	it('shows error state when playlist is missing', async () => {
		detailQuery.data = undefined;
		detailQuery.isError = true;
		detailQuery.error = new Error('404 not found');
		renderDetail('pl-bad');

		await expect.element(page.getByText("Couldn't load this playlist")).toBeVisible();
		await expect.element(page.getByText('Playlist not found')).toBeVisible();
	});

	it('shows a redacted placeholder for an admin viewing a private playlist', async () => {
		detailQuery.data = { id: 'pl-1', track_count: 9, owner_name: 'Cara', is_redacted: true };
		renderDetail('pl-1');

		await expect.element(page.getByRole('heading', { name: 'Private playlist' })).toBeVisible();
		await expect.element(page.getByText(/owned by Cara/)).toBeVisible();
	});

	it('shows empty state when playlist has no tracks', async () => {
		detailQuery.data = makePlaylist({ tracks: [], track_count: 0 });
		renderDetail('pl-1');

		await expect.element(page.getByText('This playlist is empty')).toBeVisible();
	});

	it('Play All calls playQueue with all tracks', async () => {
		detailQuery.data = makePlaylist();
		renderDetail('pl-1');

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
		renderDetail('pl-1');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();

		await page.getByRole('button', { name: /Shuffle/ }).click();

		expect(mockPlayQueue).toHaveBeenCalledOnce();
		expect(mockPlayQueue.mock.calls[0][2]).toBe(true);
	});

	it('Play All is disabled when playlist has no tracks', async () => {
		detailQuery.data = makePlaylist({ tracks: [], track_count: 0 });
		renderDetail('pl-1');

		await expect.element(page.getByText('This playlist is empty')).toBeVisible();
		const playBtn = page.getByRole('button', { name: /Play All/ });
		expect(await playBtn.element()).toBeDisabled();
	});

	it('back button is visible when playlist loads', async () => {
		detailQuery.data = makePlaylist();
		renderDetail('pl-1');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: /Go back/ })).toBeVisible();
	});

	it('owner sees the share toggle', async () => {
		detailQuery.data = makePlaylist({ is_owner: true });
		renderDetail('pl-1');

		await expect
			.element(page.getByRole('checkbox', { name: /Make playlist public/ }))
			.toBeVisible();
	});

	it('non-owner public view is read-only (no share toggle, no edit name)', async () => {
		detailQuery.data = makePlaylist({ is_owner: false, is_public: true, owner_name: 'Ann' });
		renderDetail('pl-1');

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
		renderDetail('pl-1');

		await page.getByRole('button', { name: /Edit playlist name/ }).click();
		const nameInput = page.getByPlaceholder('Playlist name');
		await expect.element(nameInput).toBeVisible();

		await userEvent.keyboard('{Escape}');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();
		expect(mockUpdatePlaylist).not.toHaveBeenCalled();
	});

	it('inline name editing: Enter saves new name', async () => {
		mockUpdatePlaylist.mockResolvedValue({ name: 'Renamed', updated_at: '2026-01-03T00:00:00Z' });
		detailQuery.data = makePlaylist();
		renderDetail('pl-1');

		await page.getByRole('button', { name: /Edit playlist name/ }).click();
		const nameInput = page.getByPlaceholder('Playlist name');
		await expect.element(nameInput).toBeVisible();
		await nameInput.clear();
		await nameInput.fill('Renamed');
		await userEvent.keyboard('{Enter}');

		expect(mockUpdatePlaylist).toHaveBeenCalledOnce();
		expect(mockUpdatePlaylist.mock.calls[0][1]).toEqual({ name: 'Renamed' });
	});

	it('calls resolvePlaylistSources after playlist loads', async () => {
		detailQuery.data = makePlaylist();
		mockResolvePlaylistSources.mockResolvedValue({});
		renderDetail('pl-1');

		await expect
			.element(page.getByRole('heading', { name: 'My Playlist', level: 1 }))
			.toBeVisible();
		await vi.waitFor(() => {
			expect(mockResolvePlaylistSources).toHaveBeenCalledWith('pl-1');
		});
	});

	it('shows play button on track hover with correct aria label', async () => {
		detailQuery.data = makePlaylist();
		renderDetail('pl-1');

		await expect.element(page.getByText('First Track')).toBeVisible();
		expect(page.getByRole('button', { name: 'Play First Track' }).elements()).toHaveLength(1);
	});

	it('play button on track calls playQueue with correct start index', async () => {
		detailQuery.data = makePlaylist();
		renderDetail('pl-1');

		await expect.element(page.getByText('Second Track')).toBeVisible();
		await page.getByRole('button', { name: 'Play Second Track' }).click();

		expect(mockPlayQueue).toHaveBeenCalledOnce();
		const [items, startIdx, shuffle] = mockPlayQueue.mock.calls[0];
		expect(items).toHaveLength(2);
		expect(startIdx).toBe(1);
		expect(shuffle).toBe(false);
	});
});
