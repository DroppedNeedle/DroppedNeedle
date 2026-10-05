import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

// keep the Request button real; stub only its mutation hook so it renders without a QueryClient
const downloadMutations = vi.hoisted(() => ({ requestMutate: vi.fn() }));
vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	requestTrack: () => ({ mutate: downloadMutations.requestMutate, isPending: false }),
	importHeldTrack: () => ({ mutate: vi.fn(), isPending: false }),
	discardHeldTrack: () => ({ mutate: vi.fn(), isPending: false }),
	reverifyHeldTrack: () => ({ mutate: vi.fn(), isPending: false })
}));

// the per-track upgrade affordance's mutation hook (QueryClient-dependent)
vi.mock('$lib/queries/downloads/UpgradeQueries.svelte', () => ({
	requestUpgradeTrack: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

// role gates the upgrade affordance (admin/trusted curators only, D18)
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

// download_client gates the Request button
vi.mock('$lib/stores/integration', () => ({
	integrationStore: {
		subscribe: (cb: (v: unknown) => void) => {
			cb({ download_client: true });
			return () => {};
		}
	}
}));

const player = vi.hoisted(() => ({ addToQueue: vi.fn(), playNext: vi.fn() }));
vi.mock('$lib/stores/player.svelte', () => ({
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
vi.mock('$lib/components/NowPlayingIndicator.svelte', emptyComponent);
vi.mock('$lib/components/TrackPlayButton.svelte', emptyComponent);
vi.mock('$lib/components/TrackPreviewButton.svelte', emptyComponent);
vi.mock('$lib/components/TrackSourceButton.svelte', emptyComponent);
vi.mock('$lib/components/JellyfinIcon.svelte', emptyComponent);
vi.mock('$lib/components/LocalFilesIcon.svelte', emptyComponent);
vi.mock('$lib/components/NavidromeIcon.svelte', emptyComponent);
vi.mock('$lib/components/PlexIcon.svelte', emptyComponent);
vi.mock('$lib/components/library/LibraryTrackRow.svelte', emptyComponent);

const blob = vi.hoisted(() => ({ download: vi.fn() }));
vi.mock('$lib/utils/blobDownload', () => ({ downloadBlob: blob.download }));

import AlbumTrackList from './AlbumTrackList.svelte';
import { getTrackContextMenuItems as realMenuItems } from './albumPlaybackHandlers';
import { buildRenderedTrackSections, buildSortedTrackMap } from './albumTrackResolvers';
import type {
	AlbumBasicInfo,
	AlbumTracksInfo,
	HeldImport,
	JellyfinTrackInfo,
	LibraryFileMeta,
	LocalAlbumMatch,
	LocalTrackInfo,
	NavidromeTrackInfo,
	PlexTrackInfo
} from '$lib/types';

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

function libTrack(over: Partial<LibraryFileMeta>): LibraryFileMeta {
	return {
		id: 'f',
		title: '',
		album_id: 'album-1',
		album_title: 'Album',
		artist_id: 'artist-1',
		artist_name: 'Artist',
		album_artist_id: 'artist-1',
		album_artist_name: 'Artist',
		musicbrainz_recording_id: null,
		musicbrainz_release_group_id: null,
		musicbrainz_artist_id: null,
		musicbrainz_album_artist_id: null,
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
		current_tier: null,
		below_cutoff: false,
		...over
	};
}

// rec-1 present by recording MBID; track 1:2 present by position only (NULL MBID)
const byRecording = new Map<string, LibraryFileMeta>([
	['rec-1', libTrack({ id: 'a', musicbrainz_recording_id: 'rec-1', track_number: 1 })]
]);
const byPosition = new Map<string, LibraryFileMeta>([
	['1:1', libTrack({ id: 'a', musicbrainz_recording_id: 'rec-1', track_number: 1 })],
	['1:2', libTrack({ id: 'b', musicbrainz_recording_id: null, track_number: 2 })]
]);

async function renderList(
	over: {
		heldByRecording?: Map<string, HeldImport>;
		heldByPosition?: Map<string, HeldImport>;
		byRecording?: Map<string, LibraryFileMeta>;
		byPosition?: Map<string, LibraryFileMeta>;
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
					jellyfin: JellyfinTrackInfo | null,
					navidrome: NavidromeTrackInfo | null,
					plex: PlexTrackInfo | null
				) => realMenuItems(track, album, local, jellyfin, navidrome, plex, null)
			: () => []
	};
	await render(AlbumTrackList, { props } as unknown as Parameters<
		typeof render<typeof AlbumTrackList>
	>[1]);
}

describe('AlbumTrackList upgrade affordance (admin/trusted, below cutoff)', () => {
	const belowCutoffOwned = new Map<string, LibraryFileMeta>([
		[
			'rec-1',
			libTrack({
				id: 'a',
				musicbrainz_recording_id: 'rec-1',
				track_number: 1,
				current_tier: 'mp3_192',
				below_cutoff: true
			})
		]
	]);

	it('shows the upgrade button to a curator for a below-cutoff owned track', async () => {
		expect.assertions(2);
		auth.role = 'trusted';
		await renderList({ byRecording: belowCutoffOwned });

		await expect.element(page.getByRole('button', { name: /upgrade/i })).toBeVisible();
		expect(page.getByRole('button', { name: /upgrade/i }).elements()).toHaveLength(1);
	});

	it('hides the upgrade button from a plain user even when below cutoff', async () => {
		expect.assertions(1);
		auth.role = 'user';
		await renderList({ byRecording: belowCutoffOwned });

		expect(page.getByRole('button', { name: /upgrade/i }).elements()).toHaveLength(0);
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
