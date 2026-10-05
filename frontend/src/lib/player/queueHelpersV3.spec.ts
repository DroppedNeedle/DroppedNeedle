import { describe, it, expect, vi } from 'vitest';

vi.mock('$lib/utils/errorHandling', () => ({
	getCoverUrl: (url: string | null, albumId: string) => url ?? `/cover/${albumId}`
}));

import type { components } from '$lib/api/v3/openapi';
import type { TrackViewV3 } from '$lib/queries/library/LibraryV3Queries.svelte';
import type { TrackMeta, TrackSourceDataV3 } from './queueHelpers';
import { buildQueueItemsFromPlexV3, buildQueueItemV3, selectBestSourceV3 } from './queueHelpers';

type RemotesTrackView = components['schemas']['RemotesTrackView'];

const baseMeta: TrackMeta = {
	albumId: 'album-1',
	albumName: 'Test Album',
	artistName: 'Artist A',
	coverUrl: '/cover.jpg',
	artistId: 'artist-1'
};

function remoteTrack(
	source: RemotesTrackView['source'],
	id: string,
	part_key?: string
): RemotesTrackView {
	return {
		source,
		id,
		title: 'Remote Song',
		album_name: 'Test Album',
		artist_name: 'Artist A',
		track_number: 3,
		disc_number: 2,
		duration_secs: 200,
		album_id: 'remote-album-1',
		image_url: '/api/v3/remotes/img/1',
		part_key: part_key ?? (source === 'plex' ? `/library/parts/1/${id}` : undefined)
	};
}

const localTrack: TrackViewV3 = {
	id: '42',
	title: 'Local Song',
	album_id: 'album-9',
	album_title: 'Local Album',
	album_artist_name: 'Artist A',
	artist_name: 'Artist A',
	artist_id: 'artist-9',
	track_number: 4,
	disc_number: 1,
	duration_seconds: 240,
	format: 'FLAC',
	file_size_bytes: 30_000_000,
	cover_available: true,
	favorite: false
};

describe('v3 album queue builders', () => {
	it('maps plex part key to stream identity and ratingKey id to scrobble key', () => {
		expect.assertions(5);
		const items = buildQueueItemsFromPlexV3([remoteTrack('plex', '777')], baseMeta);

		expect(items).toHaveLength(1);
		expect(items[0].trackSourceId).toBe('/library/parts/1/777');
		expect(items[0].plexRatingKey).toBe('777');
		expect(items[0].sourceType).toBe('plex');
		expect(items[0].streamUrl).toBe('/api/v3/stream/plex//library/parts/1/777');
	});

	it('drops plex tracks without a part key', () => {
		expect.assertions(1);
		const unstreamable = { ...remoteTrack('plex', '779'), part_key: undefined };
		const items = buildQueueItemsFromPlexV3([unstreamable], baseMeta);

		expect(items).toHaveLength(0);
	});
});

describe('buildQueueItemV3', () => {
	it('prefers local over remotes like the v1 picker', () => {
		expect.assertions(3);
		const data: TrackSourceDataV3 = {
			trackPosition: 1,
			trackTitle: 'Song',
			localTrack,
			jellyfinTrack: remoteTrack('jellyfin', 'jf-1')
		};

		const item = buildQueueItemV3(baseMeta, data);

		expect(item?.sourceType).toBe('local');
		expect(item?.sourceIds?.jellyfin).toBe('jf-1');
		expect(item?.availableSources).toEqual(['local', 'jellyfin']);
	});

	it('returns null when no source is present', () => {
		expect.assertions(2);
		const item = buildQueueItemV3(baseMeta, { trackPosition: 1, trackTitle: 'Song' });

		expect(item).toBeNull();
		expect(selectBestSourceV3({ trackPosition: 1, trackTitle: 'Song' })).toBeNull();
	});
});
