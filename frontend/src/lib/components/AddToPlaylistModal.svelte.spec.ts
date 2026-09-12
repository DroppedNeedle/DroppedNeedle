import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import AddToPlaylistModal from './AddToPlaylistModal.svelte';
import type { QueueItem } from '$lib/player/types';

const mockFetchPlaylists = vi.fn();
const mockCreatePlaylist = vi.fn();
const mockAddTracksToPlaylist = vi.fn();
const mockCheckTrackMembership = vi.fn();
const mockQueueItemToTrackData = vi.fn((item: QueueItem) => ({
	track_name: item.trackName,
	artist_name: item.artistName,
	album_name: item.albumName,
	source_type: item.sourceType
}));

vi.mock('$lib/api/playlists', () => ({
	fetchPlaylists: (...args: unknown[]) => mockFetchPlaylists(...args),
	createPlaylist: (...args: unknown[]) => mockCreatePlaylist(...args),
	addTracksToPlaylist: (...args: unknown[]) => mockAddTracksToPlaylist(...args),
	checkTrackMembership: (...args: unknown[]) => mockCheckTrackMembership(...args),
	queueItemToTrackData: (item: QueueItem) => mockQueueItemToTrackData(item),
	isRedactedPlaylist: (p: { is_redacted?: boolean } | null | undefined) => p?.is_redacted === true
}));

function makeTrack(overrides: Partial<QueueItem> = {}): QueueItem {
	return {
		trackSourceId: 'v1',
		trackName: 'Test Track',
		artistName: 'Test Artist',
		trackNumber: 1,
		albumId: 'a1',
		albumName: 'Test Album',
		coverUrl: null,
		sourceType: 'local',
		...overrides
	};
}

function makePlaylists() {
	return [
		{
			id: 'p1',
			name: 'My Playlist',
			track_count: 5,
			total_duration: 600,
			cover_urls: [],
			custom_cover_url: null,
			source_ref: null,
			created_at: '2026-01-01',
			updated_at: '2026-01-01',
			is_public: false,
			is_owner: true,
			owner_name: null,
			is_redacted: false
		},
		{
			id: 'p2',
			name: 'Another',
			track_count: 3,
			total_duration: 300,
			cover_urls: [],
			custom_cover_url: null,
			source_ref: null,
			created_at: '2026-01-02',
			updated_at: '2026-01-02',
			is_public: false,
			is_owner: true,
			owner_name: null,
			is_redacted: false
		}
	];
}

type ModalRef = { open: (tracks: QueueItem[]) => void };

async function renderModal() {
	return await render(
		AddToPlaylistModal,
		{} as Parameters<typeof render<typeof AddToPlaylistModal>>[1]
	);
}

describe('AddToPlaylistModal.svelte', () => {
	beforeEach(() => {
		mockFetchPlaylists.mockReset();
		mockCreatePlaylist.mockReset();
		mockAddTracksToPlaylist.mockReset();
		mockCheckTrackMembership.mockReset();
		mockQueueItemToTrackData.mockClear();
		mockCheckTrackMembership.mockResolvedValue({});
	});

	it('opening modal fetches playlists and renders list', async () => {
		mockFetchPlaylists.mockResolvedValue(makePlaylists());
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([makeTrack()]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		await expect.element(page.getByText('Another')).toBeVisible();
		expect(mockFetchPlaylists).toHaveBeenCalledOnce();
	});

	it('shows loading skeletons while fetching', async () => {
		let resolveFetch!: (value: unknown[]) => void;
		mockFetchPlaylists.mockReturnValue(
			new Promise<unknown[]>((r) => {
				resolveFetch = r;
			})
		);
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([makeTrack()]);

		const skeletons = page.getByTestId('playlist-skeleton').all();
		expect((await skeletons).length).toBeGreaterThan(0);

		resolveFetch(makePlaylists());
		await expect.element(page.getByText('My Playlist')).toBeVisible();
	});

	it('renders empty state when playlists list is empty', async () => {
		mockFetchPlaylists.mockResolvedValue([]);
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([makeTrack()]);

		await expect.element(page.getByText("You haven't created any playlists yet.")).toBeVisible();
	});

	it('clicking add button calls addTracksToPlaylist with correct tracks', async () => {
		mockFetchPlaylists.mockResolvedValue(makePlaylists());
		mockAddTracksToPlaylist.mockResolvedValue([]);
		const track = makeTrack();
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([track]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		const addBtn = page.getByLabelText('Add to My Playlist');
		await addBtn.click();

		await vi.waitFor(() => {
			expect(mockAddTracksToPlaylist).toHaveBeenCalledOnce();
			expect(mockAddTracksToPlaylist.mock.calls[0][0]).toBe('p1');
		});
	});

	it('after adding, button transitions from CirclePlus to Check', async () => {
		mockFetchPlaylists.mockResolvedValue(makePlaylists());
		mockAddTracksToPlaylist.mockResolvedValue([]);
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([makeTrack()]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		await page.getByLabelText('Add to My Playlist').click();

		await expect.element(page.getByLabelText('Already added').first()).toBeVisible();
	});

	it('clicking add on same playlist twice is a no-op (addedSet guard)', async () => {
		mockFetchPlaylists.mockResolvedValue(makePlaylists());
		mockAddTracksToPlaylist.mockResolvedValue([]);
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([makeTrack()]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		await page.getByLabelText('Add to My Playlist').click();
		await expect.element(page.getByLabelText('Already added').first()).toBeVisible();

		expect(mockAddTracksToPlaylist).toHaveBeenCalledOnce();
	});

	it('new playlist creation flow: creates, adds tracks, shows in list', async () => {
		mockFetchPlaylists.mockResolvedValue([]);
		mockCreatePlaylist.mockResolvedValue({
			id: 'p-new',
			name: 'Fresh',
			track_count: 0,
			total_duration: null,
			cover_urls: [],
			custom_cover_url: null,
			created_at: '2026-01-03',
			updated_at: '2026-01-03',
			tracks: []
		});
		mockAddTracksToPlaylist.mockResolvedValue([]);

		const result = await renderModal();
		(result.component as unknown as ModalRef).open([makeTrack()]);

		await expect.element(page.getByText("You haven't created any playlists yet.")).toBeVisible();

		const input = page.getByPlaceholder('New playlist name');
		await input.fill('Fresh');
		await page.getByLabelText('Create playlist').click();

		expect(mockCreatePlaylist).toHaveBeenCalledWith('Fresh');
		await expect.element(page.getByText('Fresh')).toBeVisible();
	});

	it('error during add shows error status and does not mark as added', async () => {
		mockFetchPlaylists.mockResolvedValue(makePlaylists());
		mockAddTracksToPlaylist.mockRejectedValue(new Error('Network error'));
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([makeTrack()]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		await page.getByLabelText('Add to My Playlist').click();

		await expect.element(page.getByText("Couldn't add those tracks")).toBeVisible();

		await expect.element(page.getByLabelText('Add to My Playlist')).toBeVisible();
	});

	it('shows tick for playlists where all tracks already exist', async () => {
		mockFetchPlaylists.mockResolvedValue(makePlaylists());
		mockCheckTrackMembership.mockResolvedValue({ p1: [0] });
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([makeTrack()]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		const tickBtn = page.getByLabelText('Already added').first();
		await expect.element(tickBtn).toBeVisible();
		expect(tickBtn.element().hasAttribute('disabled')).toBe(true);
	});

	it('shows partial indicator when some tracks already exist', async () => {
		mockFetchPlaylists.mockResolvedValue(makePlaylists());
		mockCheckTrackMembership.mockResolvedValue({ p1: [0] });
		const track1 = makeTrack({ trackName: 'Track 1' });
		const track2 = makeTrack({ trackName: 'Track 2', trackSourceId: 'v2' });
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([track1, track2]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		const partialBtn = page.getByLabelText('Add the remaining tracks to My Playlist');
		await expect.element(partialBtn).toBeVisible();
	});

	it('partial add only sends non-duplicate tracks', async () => {
		mockFetchPlaylists.mockResolvedValue(makePlaylists());
		mockCheckTrackMembership.mockResolvedValue({ p1: [0] });
		mockAddTracksToPlaylist.mockResolvedValue([]);
		const track1 = makeTrack({ trackName: 'Track 1' });
		const track2 = makeTrack({ trackName: 'Track 2', trackSourceId: 'v2' });
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([track1, track2]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		await page.getByLabelText('Add the remaining tracks to My Playlist').click();

		await vi.waitFor(() => {
			expect(mockAddTracksToPlaylist).toHaveBeenCalledOnce();
			const calledTracks = mockAddTracksToPlaylist.mock.calls[0][1];
			expect(calledTracks).toHaveLength(1);
			expect(calledTracks[0].track_name).toBe('Track 2');
		});
	});

	it('shows + for playlists with no overlap', async () => {
		mockFetchPlaylists.mockResolvedValue(makePlaylists());
		mockCheckTrackMembership.mockResolvedValue({});
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([makeTrack()]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		await expect.element(page.getByLabelText('Add to My Playlist')).toBeVisible();
	});
});
