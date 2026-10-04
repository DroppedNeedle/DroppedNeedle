import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: { playQueue: vi.fn() }
}));

vi.mock('$lib/utils/errorHandling', () => ({
	getCoverUrl: (url: string | null, albumId: string) => url || `/api/v1/covers/${albumId}`
}));

import type { components } from '$lib/api/v3/openapi';
import type { PlaybackMeta, QueueItem } from '$lib/player/types';
import type { TrackViewV3 } from '$lib/queries/library/LibraryV3Queries.svelte';
import { playerStore } from '$lib/stores/player.svelte';
import { launchJellyfinPlaybackV3 } from './launchJellyfinPlayback';
import { launchLocalPlaybackV3 } from './launchLocalPlayback';
import { launchNavidromePlaybackV3 } from './launchNavidromePlayback';
import { launchPlexPlaybackV3 } from './launchPlexPlayback';

type RemotesTrackView = components['schemas']['RemotesTrackView'];

const meta: PlaybackMeta = {
	albumId: 'album-1',
	albumName: 'Test Album',
	artistName: 'Test Artist',
	coverUrl: '/cover.jpg',
	artistId: 'artist-1'
};

function remoteTrack(source: RemotesTrackView['source'], id: string): RemotesTrackView {
	return {
		source,
		id,
		title: 'Remote Song',
		album_name: 'Test Album',
		artist_name: 'Test Artist',
		track_number: 2,
		disc_number: 1,
		duration_secs: 180,
		part_key: source === 'plex' ? `/library/parts/1/${id}` : undefined
	};
}

const localTrack: TrackViewV3 = {
	id: '42',
	title: 'Local Song',
	album_id: 'album-1',
	album_title: 'Test Album',
	album_artist_name: 'Test Artist',
	artist_name: 'Test Artist',
	track_number: 1,
	disc_number: 1,
	duration_seconds: 240,
	format: 'FLAC',
	file_size_bytes: 30_000_000,
	cover_available: true,
	favorite: false
};

describe('v3 launchers', () => {
	beforeEach(() => vi.clearAllMocks());

	it('launchLocalPlaybackV3 queues TrackViewV3 with local stream URL', () => {
		expect.assertions(4);
		launchLocalPlaybackV3([localTrack], 0, false, meta);

		const items: QueueItem[] = vi.mocked(playerStore.playQueue).mock.calls[0][0];
		expect(items).toHaveLength(1);
		expect(items[0].trackSourceId).toBe('42');
		expect(items[0].streamUrl).toBe('/api/v3/stream/local/42');
		expect(items[0].format).toBe('flac');
	});

	it('launchJellyfinPlaybackV3 queues RemotesTrackView ids', () => {
		expect.assertions(3);
		launchJellyfinPlaybackV3([remoteTrack('jellyfin', 'jf-1')], 0, true, meta);

		const call = vi.mocked(playerStore.playQueue).mock.calls[0];
		const items: QueueItem[] = call[0];
		expect(items[0].streamUrl).toBe('/api/v3/stream/jellyfin/jf-1');
		expect(items[0].format).toBe('aac');
		expect(call[2]).toBe(true);
	});

	it('launchNavidromePlaybackV3 queues RemotesTrackView ids', () => {
		expect.assertions(2);
		launchNavidromePlaybackV3([remoteTrack('navidrome', 'nd-1')], 0, false, meta);

		const items: QueueItem[] = vi.mocked(playerStore.playQueue).mock.calls[0][0];
		expect(items[0].trackSourceId).toBe('nd-1');
		expect(items[0].sourceType).toBe('navidrome');
	});

	it('launchPlexPlaybackV3 streams by part key and scrobbles by ratingKey id', () => {
		expect.assertions(4);
		launchPlexPlaybackV3([remoteTrack('plex', '777')], 0, false, meta);

		const items: QueueItem[] = vi.mocked(playerStore.playQueue).mock.calls[0][0];
		expect(items[0].trackSourceId).toBe('/library/parts/1/777');
		expect(items[0].streamUrl).toBe('/api/v3/stream/plex//library/parts/1/777');
		expect(items[0].plexRatingKey).toBe('777');
		expect(items[0].sourceType).toBe('plex');
	});

	it('launchPlexPlaybackV3 clamps startIndex and skips empty lists', () => {
		expect.assertions(2);
		launchPlexPlaybackV3([], 0, false, meta);
		expect(vi.mocked(playerStore.playQueue)).not.toHaveBeenCalled();

		launchPlexPlaybackV3([remoteTrack('plex', '777')], 9, false, meta);
		expect(vi.mocked(playerStore.playQueue).mock.calls[0][1]).toBe(0);
	});
});
