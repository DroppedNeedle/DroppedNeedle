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

vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: { isPlaying: false, nowPlaying: null, currentQueueItem: null }
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
vi.mock('$lib/components/ContextMenu.svelte', emptyComponent);
vi.mock('$lib/components/JellyfinIcon.svelte', emptyComponent);
vi.mock('$lib/components/LocalFilesIcon.svelte', emptyComponent);
vi.mock('$lib/components/NavidromeIcon.svelte', emptyComponent);
vi.mock('$lib/components/PlexIcon.svelte', emptyComponent);
vi.mock('$lib/components/library/LibraryTrackRow.svelte', emptyComponent);

import AlbumTrackList from './AlbumTrackList.svelte';
import { buildRenderedTrackSections } from './albumTrackResolvers';
import type { AlbumBasicInfo, AlbumTracksInfo, HeldImport, LibraryFileMeta } from '$lib/types';

function heldFor(recording_mbid: string): HeldImport {
	return {
		id: 1,
		release_group_mbid: 'rg-1',
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid,
		track_number: 3,
		disc_number: 1,
		track_title: 'Genuinely Missing',
		artist_name: 'Artist',
		album_title: 'Album',
		year: null,
		original_filename: 'x.flac',
		file_format: 'flac',
		duration_seconds: 100,
		expected_duration_seconds: 100,
		reason: 'fingerprint_mismatch',
		reason_detail: null,
		source: 'usenet',
		source_task_id: 't',
		created_at: 0,
		evidence_title: 'Other Song',
		evidence_artist: 'Other Artist',
		evidence_score: 0.9,
		management_retry_count: 0,
		management_next_retry_at: null
	};
}

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

function renderList(
	over: {
		heldByRecording?: Map<string, HeldImport>;
		heldByPosition?: Map<string, HeldImport>;
		byRecording?: Map<string, LibraryFileMeta>;
		releaseMbid?: string | null;
	} = {}
) {
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
		renderedTrackSections: buildRenderedTrackSections(TRACKS),
		trackLinkMap: new Map(),
		jellyfinMatch: null,
		localMatch: null,
		navidromeMatch: null,
		plexMatch: null,
		jellyfinTrackMap: new Map(),
		localTrackMap: new Map(),
		navidromeTrackMap: new Map(),
		plexTrackMap: new Map(),
		jellyfinTracks: [],
		localTracks: [],
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
		libraryTracksByPosition: byPosition,
		heldByRecording: over.heldByRecording ?? new Map(),
		heldByPosition: over.heldByPosition ?? new Map(),
		releaseGroupMbid: 'rg-1',
		releaseMbid: over.releaseMbid ?? null,
		onPlaySourceTrack: vi.fn(),
		onTrackGenerated: vi.fn(),
		onQuotaUpdate: vi.fn(),
		getTrackContextMenuItems: () => []
	};
	render(AlbumTrackList, { props } as unknown as Parameters<
		typeof render<typeof AlbumTrackList>
	>[1]);
}

describe('AlbumTrackList in-library detection', () => {
	it('shows the Request button only for the genuinely-missing track', async () => {
		expect.assertions(2);
		renderList();

		// matched rows are hidden, leaving exactly one Request button for the genuinely missing track
		await expect.element(page.getByText('Genuinely Missing')).toBeVisible();
		const requestButtons = page.getByRole('button', { name: 'Request this track' }).elements();
		expect(requestButtons).toHaveLength(1);
	});

	it('shows a "held" chip (not Request) for an un-owned track with a held candidate', async () => {
		expect.assertions(2);
		renderList({ heldByRecording: new Map([['rec-3', heldFor('rec-3')]]) });

		// the genuinely-missing track now has a held candidate -> the held review chip appears...
		await expect.element(page.getByRole('button', { name: /held/i })).toBeVisible();
		// ...and it replaces the Request button for that track (nothing left to request)
		expect(page.getByRole('button', { name: 'Request this track' }).elements()).toHaveLength(0);
	});
});

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
		renderList({ byRecording: belowCutoffOwned });

		await expect.element(page.getByRole('button', { name: /upgrade/i })).toBeVisible();
		expect(page.getByRole('button', { name: /upgrade/i }).elements()).toHaveLength(1);
	});

	it('hides the upgrade button from a plain user even when below cutoff', async () => {
		expect.assertions(1);
		auth.role = 'user';
		renderList({ byRecording: belowCutoffOwned });

		expect(page.getByRole('button', { name: /upgrade/i }).elements()).toHaveLength(0);
	});

	it('hides the upgrade button when the track meets the cutoff', async () => {
		expect.assertions(1);
		auth.role = 'admin';
		renderList(); // default fixtures: below_cutoff false everywhere

		expect(page.getByRole('button', { name: /upgrade/i }).elements()).toHaveLength(0);
	});
});

describe('AlbumTrackList exact-track request release propagation', () => {
	it('sends the displayed selected edition to the track request mutation', async () => {
		downloadMutations.requestMutate.mockClear();
		renderList({ releaseMbid: 'release-20' });

		await page.getByRole('button', { name: 'Request this track' }).click();
		expect(downloadMutations.requestMutate).toHaveBeenCalledTimes(1);
		expect(downloadMutations.requestMutate.mock.calls[0][0]).toMatchObject({
			recording_mbid: 'rec-3',
			release_group_mbid: 'rg-1',
			release_id: 'release-20'
		});
	});

	it('keeps the track request usable with a null edition', async () => {
		downloadMutations.requestMutate.mockClear();
		renderList();

		await page.getByRole('button', { name: 'Request this track' }).click();
		expect(downloadMutations.requestMutate).toHaveBeenCalledTimes(1);
		expect(downloadMutations.requestMutate.mock.calls[0][0].release_id).toBeNull();
	});
});
