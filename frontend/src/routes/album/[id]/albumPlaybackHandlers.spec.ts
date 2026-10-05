import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { AlbumBasicInfo, LocalTrackInfo } from '$lib/types';

const mocks = vi.hoisted(() => ({
	launchLocalPlayback: vi.fn(),
	addMultipleToQueue: vi.fn(),
	playMultipleNext: vi.fn()
}));

vi.mock('$lib/player/launchLocalPlayback', () => ({
	launchLocalPlayback: mocks.launchLocalPlayback
}));
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		addToQueue: vi.fn(),
		playNext: vi.fn(),
		addMultipleToQueue: mocks.addMultipleToQueue,
		playMultipleNext: mocks.playMultipleNext
	}
}));
const blob = vi.hoisted(() => ({ download: vi.fn() }));
vi.mock('$lib/utils/blobDownload', () => ({ downloadBlob: blob.download }));
const toast = vi.hoisted(() => ({ show: vi.fn() }));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: toast.show } }));

import { buildLocalAlbumDownloadCallback, getTrackContextMenuItems } from './albumPlaybackHandlers';

const album: AlbumBasicInfo = {
	title: 'Avalon',
	musicbrainz_id: 'release-group-1',
	artist_name: 'Anthony Green',
	artist_id: 'artist-1',
	in_library: true,
	cover_url: null
};

const localTracks: LocalTrackInfo[] = [
	{
		track_file_id: 'file-1',
		title: 'She Loves Me So',
		track_number: 1,
		disc_number: 1,
		duration_seconds: 233,
		size_bytes: 1_000,
		format: 'FLAC'
	},
	{
		track_file_id: 'file-14',
		title: 'The Fisherman Will Be Bewildered (H&D EP Version)',
		track_number: 14,
		disc_number: 1,
		duration_seconds: 212,
		size_bytes: 1_000,
		format: 'FLAC'
	}
];

describe('track context menu Download item', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		blob.download.mockResolvedValue(undefined);
	});

	it('omits the Download item when downloads are restricted', () => {
		const items = getTrackContextMenuItems(
			{ position: 1, disc_number: 1, title: 'She Loves Me So' },
			album,
			localTracks[0],
			null,
			null,
			null,
			null,
			false
		);
		expect(items.map((item) => item.label)).toEqual([
			'Add to Queue',
			'Play Next',
			'Add to Playlist'
		]);
	});
});

describe('track context menu Remove file item', () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	function itemsFor(onRemoveLocalFile?: (fileId: string) => void) {
		return getTrackContextMenuItems(
			{ position: 1, disc_number: 1, title: 'She Loves Me So' },
			album,
			localTracks[0],
			null,
			null,
			null,
			null,
			true,
			onRemoveLocalFile
		);
	}

	it('omits the Remove file item when no callback is supplied (viewer not trusted)', () => {
		expect(itemsFor().map((item) => item.label)).not.toContain('Remove file');
	});

	it('opens the confirm dialog for the resolved local file', () => {
		const onRemoveLocalFile = vi.fn();
		const remove = itemsFor(onRemoveLocalFile).find((item) => item.label === 'Remove file');
		expect(remove).toBeDefined();

		remove!.onclick();

		expect(onRemoveLocalFile).toHaveBeenCalledWith('file-1');
	});
});

describe('buildLocalAlbumDownloadCallback', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		blob.download.mockResolvedValue(undefined);
	});

	it('returns undefined when downloads are restricted', () => {
		expect.assertions(1);
		expect(buildLocalAlbumDownloadCallback('mbid-1', false)).toBeUndefined();
	});
});
