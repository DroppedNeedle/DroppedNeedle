import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import AddToPlaylistModal from './AddToPlaylistModal.svelte';
import type { QueueItem } from '$lib/player/types';

const mockV3List = vi.fn();
const mockCreateMutate = vi.fn();
const mockAddTracksMutate = vi.fn();
const mockCheckTracksMutate = vi.fn();
const mockQueueItemToTrackData = vi.fn((item: QueueItem) => ({
	track_name: item.trackName,
	artist_name: item.artistName,
	album_name: item.albumName,
	source_type: item.sourceType
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: (...args: unknown[]) => mockV3List(...args) } } }
}));

vi.mock('$lib/queries/playlists/PlaylistV3Mutations.svelte', () => ({
	createPlaylistV3: () => ({ mutateAsync: mockCreateMutate, isPending: false }),
	addPlaylistTracksV3: () => ({ mutateAsync: mockAddTracksMutate, isPending: false }),
	checkPlaylistTracksV3: () => ({ mutateAsync: mockCheckTracksMutate, isPending: false })
}));

vi.mock('$lib/api/playlists', () => ({
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
			created_at: 1767225600,
			updated_at: 1767225600,
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
			created_at: 1767312000,
			updated_at: 1767312000,
			is_public: false,
			is_owner: true,
			owner_name: null,
			is_redacted: false
		}
	];
}

function mockListAnswer(playlists: unknown[]) {
	mockV3List.mockResolvedValue({ playlists });
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
		mockV3List.mockReset();
		mockCreateMutate.mockReset();
		mockAddTracksMutate.mockReset();
		mockCheckTracksMutate.mockReset();
		mockQueueItemToTrackData.mockClear();
		mockCheckTracksMutate.mockResolvedValue({ membership: {} });
	});

	it('clicking add on same playlist twice is a no-op (addedSet guard)', async () => {
		mockListAnswer(makePlaylists());
		mockAddTracksMutate.mockResolvedValue({ tracks: [] });
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([makeTrack()]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		await page.getByLabelText('Add to My Playlist').click();
		await expect.element(page.getByLabelText('Already added').first()).toBeVisible();

		expect(mockAddTracksMutate).toHaveBeenCalledOnce();
	});

	it('error during add shows error status and does not mark as added', async () => {
		mockListAnswer(makePlaylists());
		mockAddTracksMutate.mockRejectedValue(new Error('Network error'));
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([makeTrack()]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		await page.getByLabelText('Add to My Playlist').click();

		await expect.element(page.getByText("Couldn't add those tracks")).toBeVisible();

		await expect.element(page.getByLabelText('Add to My Playlist')).toBeVisible();
	});

	it('partial add only sends non-duplicate tracks', async () => {
		mockListAnswer(makePlaylists());
		mockCheckTracksMutate.mockResolvedValue({ membership: { '0': ['p1'] } });
		mockAddTracksMutate.mockResolvedValue({ tracks: [] });
		const track1 = makeTrack({ trackName: 'Track 1' });
		const track2 = makeTrack({ trackName: 'Track 2', trackSourceId: 'v2' });
		const result = await renderModal();
		(result.component as unknown as ModalRef).open([track1, track2]);

		await expect.element(page.getByText('My Playlist')).toBeVisible();
		await page.getByLabelText('Add the remaining tracks to My Playlist').click();

		await vi.waitFor(() => {
			expect(mockAddTracksMutate).toHaveBeenCalledOnce();
			const calledTracks = mockAddTracksMutate.mock.calls[0][0].tracks;
			expect(calledTracks).toHaveLength(1);
			expect(calledTracks[0].track_name).toBe('Track 2');
		});
	});
});
