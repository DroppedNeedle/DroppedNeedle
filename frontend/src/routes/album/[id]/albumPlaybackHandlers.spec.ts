import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { AlbumBasicInfo, LocalAlbumMatch, LocalTrackInfo, Track } from '$lib/types';
import type { QueueItem } from '$lib/player/types';

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

import {
	buildLocalAlbumDownloadCallback,
	buildSourceCallbacks,
	getTrackContextMenuItems,
	playSourceTrack
} from './albumPlaybackHandlers';

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

const localMatch: LocalAlbumMatch = {
	found: true,
	musicbrainz_id: album.musicbrainz_id,
	tracks: localTracks,
	total_size_bytes: 2_000,
	primary_format: 'FLAC'
};

const canonicalTracks: Track[] = [
	{ position: 1, disc_number: 1, title: 'She Loves Me So' },
	{ position: 14, disc_number: 1, title: 'The Fisherman Will Be Bewildered' }
];

const tracksGetters = {
	jellyfin: () => [],
	local: () => localTracks,
	navidrome: () => [],
	plex: () => []
};

describe('album playback canonical titles', () => {
	beforeEach(() => vi.clearAllMocks());

	it('keeps the release-track title when a local album row starts playback', () => {
		playSourceTrack(
			'local',
			14,
			1,
			'The Fisherman Will Be Bewildered',
			album,
			null,
			localMatch,
			null,
			null,
			canonicalTracks
		);

		expect(mocks.launchLocalPlayback).toHaveBeenCalledTimes(1);
		const [tracks, startIndex] = mocks.launchLocalPlayback.mock.calls[0] as [
			LocalTrackInfo[],
			number
		];
		expect(startIndex).toBe(1);
		expect(tracks[1]).toMatchObject({
			track_file_id: 'file-14',
			title: 'The Fisherman Will Be Bewildered'
		});
		expect(localTracks[1].title).toBe('The Fisherman Will Be Bewildered (H&D EP Version)');
	});

	it('uses canonical titles for every album-level local queue action', () => {
		const launcher = vi.fn();
		const openPlaylist = vi.fn();
		const callbacks = buildSourceCallbacks(
			() => localMatch,
			launcher,
			'local',
			() => album,
			tracksGetters,
			() => canonicalTracks,
			() => ({ open: openPlaylist })
		);

		callbacks.onPlayAll();
		callbacks.onShuffle();
		callbacks.onAddAllToQueue();
		callbacks.onPlayAllNext();
		callbacks.onAddAllToPlaylist();

		expect(launcher).toHaveBeenCalledTimes(2);
		for (const call of launcher.mock.calls) {
			const tracks = call[0] as LocalTrackInfo[];
			expect(tracks[1].title).toBe('The Fisherman Will Be Bewildered');
		}
		const queued = mocks.addMultipleToQueue.mock.calls[0][0] as QueueItem[];
		const next = mocks.playMultipleNext.mock.calls[0][0] as QueueItem[];
		const playlist = openPlaylist.mock.calls[0][0] as QueueItem[];
		expect(queued[1].trackName).toBe('The Fisherman Will Be Bewildered');
		expect(next[1].trackName).toBe('The Fisherman Will Be Bewildered');
		expect(playlist[1].trackName).toBe('The Fisherman Will Be Bewildered');
	});
});

describe('track context menu Download item', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		blob.download.mockResolvedValue(undefined);
	});

	function itemsFor(file: LocalTrackInfo | null) {
		return getTrackContextMenuItems(
			{ position: 1, disc_number: 1, title: 'She Loves Me So' },
			album,
			file,
			null,
			null,
			null,
			null
		);
	}

	it('downloads the local file via blob with a success toast', async () => {
		const download = itemsFor(localTracks[0]).find((item) => item.label === 'Download');
		expect(download).toBeDefined();

		download!.onclick();
		await vi.waitFor(() => {
			expect(blob.download).toHaveBeenCalledWith('/api/v1/download/local/track/file-1');
		});
		expect(toast.show).toHaveBeenCalledWith(expect.objectContaining({ type: 'success' }));
	});

	it('toasts a user-safe error when the blob download fails', async () => {
		blob.download.mockRejectedValueOnce(new Error('gone'));
		const download = itemsFor(localTracks[0]).find((item) => item.label === 'Download');

		download!.onclick();
		await vi.waitFor(() => {
			expect(toast.show).toHaveBeenCalledWith(expect.objectContaining({ type: 'error' }));
		});
		const messages = toast.show.mock.calls.map((call) => String(call[0].message));
		expect(messages.every((message) => !message.includes('/api/v1/download'))).toBe(true);
	});

	it('omits the Download item when no local file is resolved', () => {
		const labels = itemsFor(null).map((item) => item.label);
		expect(labels).toEqual(['Add to Queue', 'Play Next', 'Add to Playlist']);
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

	it('omits the Remove file item when no local file is resolved', () => {
		const items = getTrackContextMenuItems(
			{ position: 1, disc_number: 1, title: 'She Loves Me So' },
			album,
			null,
			null,
			null,
			null,
			null,
			true,
			vi.fn()
		);
		expect(items.map((item) => item.label)).not.toContain('Remove file');
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

	it('returns undefined without a local match MBID', () => {
		expect.assertions(2);
		expect(buildLocalAlbumDownloadCallback(null, true)).toBeUndefined();
		expect(buildLocalAlbumDownloadCallback(undefined, true)).toBeUndefined();
	});

	it('returns undefined when downloads are restricted', () => {
		expect.assertions(1);
		expect(buildLocalAlbumDownloadCallback('mbid-1', false)).toBeUndefined();
	});

	it('downloads the album zip by MBID', async () => {
		expect.assertions(2);
		const callback = buildLocalAlbumDownloadCallback('mbid-1', true);
		expect(callback).toBeDefined();

		callback!();
		await vi.waitFor(() => {
			expect(blob.download).toHaveBeenCalledWith('/api/v1/download/local/album/mbid/mbid-1');
		});
	});
});
