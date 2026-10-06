import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { LocalAlbumSummary } from '$lib/types';
import type { LibraryAdapter } from './libraryController.svelte';

const actions = vi.hoisted(() => ({ downloadAlbum: vi.fn() }));
vi.mock('$lib/utils/downloadActions', () => ({
	downloadAlbumArchive: actions.downloadAlbum,
	downloadTrackFile: vi.fn()
}));

import { createLibraryController } from './libraryController.svelte';

const album: LocalAlbumSummary = {
	musicbrainz_id: 'mbid-1',
	name: 'Avalon',
	artist_name: 'Roxy Music',
	track_count: 10,
	total_size_bytes: 1000
};

function stubAdapter(sourceType: LibraryAdapter<LocalAlbumSummary>['sourceType']) {
	return createLibraryController<LocalAlbumSummary>({
		sourceType,
		getAlbumId: (entry) => entry.musicbrainz_id,
		getAlbumName: (entry) => entry.name,
		getArtistName: (entry) => entry.artist_name,
		getAlbumMbid: (entry) => entry.musicbrainz_id,
		getAlbumImageUrl: () => null,
		getAlbumYear: (entry) => entry.year ?? null,
		fetchAlbums: async () => ({ items: [], total: 0 }),
		fetchSidebarData: async (_signal, current) => ({ data: current, hasFreshData: false }),
		fetchAlbumQueueItems: async () => [],
		launchPlayback: async () => {},
		getAlbumsListCached: () => null,
		setAlbumsListCached: () => {},
		isAlbumsListCacheStale: () => true,
		getSidebarCached: () => null,
		setSidebarCached: () => {},
		isSidebarCacheStale: () => true,
		sortOptions: [{ value: 'name', label: 'Name' }],
		defaultSortBy: 'name',
		ascValue: 'asc',
		descValue: 'desc',
		getDefaultSortOrder: () => 'asc',
		supportsGenres: false,
		supportsMoods: false,
		supportsDecades: false,
		supportsTags: false,
		supportsFavorites: false,
		supportsShuffle: false,
		errorMessage: 'Failed to load albums'
	});
}

beforeEach(() => {
	vi.clearAllMocks();
});

describe('library controller Download Album item', () => {
	it('downloads the album zip for an allowed local album', () => {
		expect.assertions(3);
		const controller = stubAdapter('local');
		const download = controller
			.getAlbumMenuItems({ ...album, download_allowed: true })
			.find((item) => item.label === 'Download Album');

		expect(download).toBeDefined();
		download!.onclick();
		expect(actions.downloadAlbum).toHaveBeenCalledTimes(1);
		expect(actions.downloadAlbum).toHaveBeenCalledWith('/api/v3/download/local/album/mbid/mbid-1');
	});

	it('keeps the item when the permission bit is missing (fail-open)', () => {
		expect.assertions(1);
		const controller = stubAdapter('local');

		const labels = controller.getAlbumMenuItems(album).map((item) => item.label);

		expect(labels).toContain('Download Album');
	});

	it('omits the item when downloads are restricted', () => {
		expect.assertions(1);
		const controller = stubAdapter('local');

		const labels = controller
			.getAlbumMenuItems({ ...album, download_allowed: false })
			.map((item) => item.label);

		expect(labels).toEqual(['Add to Queue', 'Play Next', 'Add to Playlist']);
	});

	it('omits the item for non-local sources', () => {
		expect.assertions(1);
		const controller = stubAdapter('jellyfin');

		const labels = controller.getAlbumMenuItems(album).map((item) => item.label);

		expect(labels).toEqual(['Add to Queue', 'Play Next', 'Add to Playlist']);
	});
});
