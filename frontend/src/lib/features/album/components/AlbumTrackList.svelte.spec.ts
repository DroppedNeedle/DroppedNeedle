import { page } from 'vitest/browser';
import { beforeAll, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

// keep the Request button real; stub only its mutation hook so it renders without a QueryClient
const downloadMutations = vi.hoisted(() => ({ requestMutate: vi.fn() }));
vi.mock('$lib/features/downloads/queries/DownloadMutations.svelte', () => ({
	createRequestTrackMutation: () => ({ mutate: downloadMutations.requestMutate, isPending: false })
}));

// the per-track upgrade affordance's mutation hook (QueryClient-dependent)
vi.mock('$lib/features/downloads/queries/UpgradeQueries.svelte', () => ({
	createRequestUpgradeTrackMutation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

// role gates the upgrade affordance (admin/trusted curators only)
const auth = vi.hoisted(() => ({ role: 'user' }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: {
		get isAdmin() {
			return auth.role === 'admin';
		},
		get isTrusted() {
			return auth.role === 'trusted' || auth.role === 'admin';
		}
	}
}));

const player = vi.hoisted(() => ({ addToQueue: vi.fn(), playNext: vi.fn() }));
vi.mock('$lib/features/player/state/player.svelte', () => ({
	playerStore: {
		isPlaying: false,
		nowPlaying: null,
		currentQueueItem: null,
		addToQueue: player.addToQueue,
		playNext: player.playNext
	}
}));

// heavy / QueryClient-dependent children not under test
const { emptyComponent } = vi.hoisted(() => ({
	emptyComponent: () => {
		const Comp = function () {};
		Comp.prototype = {};
		return { default: Comp };
	}
}));
vi.mock('$lib/features/player/components/NowPlayingIndicator.svelte', emptyComponent);
vi.mock('$lib/features/album/components/TrackPlayButton.svelte', emptyComponent);
vi.mock('$lib/components/TrackPreviewButton.svelte', emptyComponent);
vi.mock('$lib/features/album/components/TrackSourceButton.svelte', emptyComponent);
vi.mock('$lib/media/icons/JellyfinIcon.svelte', emptyComponent);
vi.mock('$lib/media/icons/LocalFilesIcon.svelte', emptyComponent);
vi.mock('$lib/media/icons/NavidromeIcon.svelte', emptyComponent);
vi.mock('$lib/media/icons/PlexIcon.svelte', emptyComponent);
vi.mock('$lib/features/library/components/LibraryTrackRow.svelte', emptyComponent);

const blob = vi.hoisted(() => ({ download: vi.fn() }));
vi.mock('$lib/utils/blobDownload', () => ({ downloadBlob: blob.download }));

import AlbumTrackList from './AlbumTrackList.svelte';
import { HomeQueryKeyFactory } from '$lib/queries/featureKeys';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { getTrackContextMenuItems as realMenuItems } from './albumPlaybackHandlers';
import { buildRenderedTrackSections, buildSortedTrackMap } from './albumTrackResolvers';
import type {
	AlbumBasicInfo,
	AlbumTracksInfo,
	HeldFileInEntry,
	LibraryStatusTrack,
	LocalAlbumMatch,
	LocalTrackInfo
} from '$lib/features/album/queries/types';
import type { RemoteTrack } from '$lib/features/remote-library/queries/types';

const TRACKS: AlbumTracksInfo['tracks'] = [
	{ position: 1, disc_number: 1, title: 'Matched By MBID', length: 100000, recording_id: 'rec-1' },
	{
		position: 2,
		disc_number: 1,
		title: 'Matched By Position',
		length: 100000,
		recording_id: 'rec-2'
	},
	{ position: 3, disc_number: 1, title: 'Genuinely Missing', length: 100000, recording_id: 'rec-3' }
];

function libTrack(over: Partial<LibraryStatusTrack>): LibraryStatusTrack {
	return {
		id: 'f',
		title: '',
		album_id: 'album-1',
		album_title: 'Album',
		artist_id: 'artist-1',
		artist_name: 'Artist',
		album_artist_id: 'artist-1',
		album_artist_name: 'Artist',
		recording_mbid: null,
		release_group_mbid: null,
		artist_mbid: null,
		album_artist_mbid: null,
		disc_number: 1,
		track_number: 0,
		year: null,
		genre: null,
		format: 'flac',
		bit_rate: null,
		sample_rate: null,
		bit_depth: null,
		channels: null,
		duration_seconds: 0,
		file_size_bytes: 1,
		date_added: 1,
		cover_available: false,
		art_missing: false,
		favorite: false,
		row_revision: 1,
		current_tier: 'lossless',
		below_cutoff: false,
		...over
	};
}

// rec-1 present by recording MBID; track 1:2 present by position only (NULL MBID)
const byRecording = new Map<string, LibraryStatusTrack>([
	['rec-1', libTrack({ id: 'a', recording_mbid: 'rec-1', track_number: 1 })]
]);
const byPosition = new Map<string, LibraryStatusTrack>([
	['1:1', libTrack({ id: 'a', recording_mbid: 'rec-1', track_number: 1 })],
	['1:2', libTrack({ id: 'b', recording_mbid: null, track_number: 2 })]
]);

async function renderList(
	over: {
		heldByRecording?: Map<string, HeldFileInEntry>;
		heldByPosition?: Map<string, HeldFileInEntry>;
		byRecording?: Map<string, LibraryStatusTrack>;
		byPosition?: Map<string, LibraryStatusTrack>;
		releaseMbid?: string | null;
		tracks?: AlbumTracksInfo['tracks'];
		localTracks?: LocalTrackInfo[];
		useRealMenuItems?: boolean;
	} = {}
) {
	const localTracks = over.localTracks ?? [];
	const localMatch: LocalAlbumMatch | null =
		localTracks.length > 0
			? {
					found: true,
					tracks: localTracks,
					total_size_bytes: localTracks.reduce((sum, t) => sum + t.size_bytes, 0)
				}
			: null;
	const album: AlbumBasicInfo = {
		musicbrainz_id: 'rg-1',
		artist_name: 'Artist',
		title: 'Album',
		cover_url: null,
		artist_id: 'art-1',
		in_library: false
	};
	const props = {
		album,
		renderedTrackSections: buildRenderedTrackSections(over.tracks ?? TRACKS),
		trackLinkMap: new Map(),
		jellyfinMatch: null,
		localMatch,
		navidromeMatch: null,
		plexMatch: null,
		jellyfinTrackMap: new Map(),
		localTrackMap: buildSortedTrackMap(localTracks),
		navidromeTrackMap: new Map(),
		plexTrackMap: new Map(),
		jellyfinTracks: [],
		localTracks,
		navidromeTracks: [],
		plexTracks: [],
		trackLinks: [],
		youtubeEnabled: false,
		youtubeApiConfigured: false,
		previewCacheMap: new Map(),
		jellyfinEnabled: false,
		localfilesEnabled: false,
		navidromeEnabled: false,
		plexEnabled: false,
		libraryTracksByRecording: over.byRecording ?? byRecording,
		libraryTracksByPosition: over.byPosition ?? byPosition,
		heldByRecording: over.heldByRecording ?? new Map(),
		heldByPosition: over.heldByPosition ?? new Map(),
		releaseGroupMbid: 'rg-1',
		releaseMbid: over.releaseMbid ?? null,
		onPlaySourceTrack: vi.fn(),
		onTrackGenerated: vi.fn(),
		onQuotaUpdate: vi.fn(),
		getTrackContextMenuItems: over.useRealMenuItems
			? (
					track: { position: number; disc_number?: number | null; title: string },
					local: LocalTrackInfo | null,
					jellyfin: RemoteTrack | null,
					navidrome: RemoteTrack | null,
					plex: RemoteTrack | null
				) => realMenuItems(track, album, local, jellyfin, navidrome, plex, null)
			: () => []
	};
	await render(AlbumTrackList, { props } as unknown as Parameters<
		typeof render<typeof AlbumTrackList>
	>[1]);
}

// A download source is set up, which the Upgrade button needs.
beforeAll(async () => {
	await setQueryDataWithPersister(HomeQueryKeyFactory.integrationStatus(undefined), {
		download_client: true,
		jellyfin: false,
		lastfm: false,
		library: false,
		listenbrainz: false,
		localfiles: false,
		navidrome: false,
		plex: false,
		youtube: false,
		youtube_api: false
	});
});

describe('AlbumTrackList exact-track request release propagation', () => {
	it('sends the displayed selected edition to the track request mutation', async () => {
		downloadMutations.requestMutate.mockClear();
		await renderList({ releaseMbid: 'release-20' });

		await page.getByRole('button', { name: 'Request this track' }).click();
		expect(downloadMutations.requestMutate).toHaveBeenCalledTimes(1);
		expect(downloadMutations.requestMutate.mock.calls[0][0]).toMatchObject({
			recording_mbid: 'rec-3',
			release_group_mbid: 'rg-1',
			release_id: 'release-20'
		});
	});
});
