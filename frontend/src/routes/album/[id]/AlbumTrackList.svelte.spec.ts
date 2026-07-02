import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

// keep the Request button real; stub only its mutation hook so it renders without a QueryClient
vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	requestTrack: () => ({ mutate: vi.fn(), isPending: false }),
	importHeldTrack: () => ({ mutate: vi.fn(), isPending: false }),
	discardHeldTrack: () => ({ mutate: vi.fn(), isPending: false })
}));

// the per-track upgrade affordance's mutation hook (QueryClient-dependent)
vi.mock('$lib/queries/downloads/UpgradeQueries.svelte', () => ({
	requestUpgradeTrack: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

// role gates the upgrade affordance (admin/trusted curators only, D18)
const auth = vi.hoisted(() => ({ role: 'user' }));
vi.mock('$lib/stores/authStore.svelte', () => ({
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
import type { HeldImport, LibraryFileMeta } from '$lib/types';

function heldFor(recording_mbid: string): HeldImport {
	return {
		id: 1,
		release_group_mbid: 'rg-1',
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
		reason: 'fingerprint_mismatch',
		source: 'usenet',
		source_task_id: 't',
		created_at: 0,
		evidence_title: 'Other Song',
		evidence_artist: 'Other Artist',
		evidence_score: 0.9
	};
}

const TRACKS = [
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
		recording_mbid: null,
		disc_number: 1,
		track_number: 0,
		track_title: '',
		artist_name: 'Artist',
		file_path: '/x.flac',
		file_format: 'flac',
		bit_rate: null,
		sample_rate: null,
		bit_depth: null,
		duration_seconds: null,
		file_size_bytes: 1,
		current_tier: null,
		below_cutoff: false,
		...over
	};
}

// rec-1 present by recording MBID; track 1:2 present by position only (NULL MBID)
const byRecording = new Map<string, LibraryFileMeta>([
	['rec-1', libTrack({ id: 'a', recording_mbid: 'rec-1', track_number: 1 })]
]);
const byPosition = new Map<string, LibraryFileMeta>([
	['1:1', libTrack({ id: 'a', recording_mbid: 'rec-1', track_number: 1 })],
	['1:2', libTrack({ id: 'b', recording_mbid: null, track_number: 2 })]
]);

function renderList(
	over: {
		heldByRecording?: Map<string, HeldImport>;
		heldByPosition?: Map<string, HeldImport>;
		byRecording?: Map<string, LibraryFileMeta>;
	} = {}
) {
	const album = {
		musicbrainz_id: 'rg-1',
		artist_name: 'Artist',
		title: 'Album',
		cover_url: null,
		artist_id: 'art-1'
	};
	const props = {
		// eslint-disable-next-line @typescript-eslint/no-explicit-any -- minimal album stub for the test
		album: album as any,
		renderedTrackSections: buildRenderedTrackSections(
			// eslint-disable-next-line @typescript-eslint/no-explicit-any -- minimal MB track stubs
			TRACKS as any
		),
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
				recording_mbid: 'rec-1',
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
