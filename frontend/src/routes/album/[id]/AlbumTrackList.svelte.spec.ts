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
import { closeAllMenus } from '$lib/components/ContextMenu.svelte';
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

describe('AlbumTrackList in-library detection', () => {
	it('shows the Request button only for the genuinely-missing track', async () => {
		expect.assertions(2);
		await renderList();

		// matched rows are hidden, leaving exactly one Request button for the genuinely missing track
		await expect.element(page.getByText('Genuinely Missing')).toBeVisible();
		const requestButtons = page.getByRole('button', { name: 'Request this track' }).elements();
		expect(requestButtons).toHaveLength(1);
	});

	it('shows a "held" chip (not Request) for an un-owned track with a held candidate', async () => {
		expect.assertions(2);
		await renderList({ heldByRecording: new Map([['rec-3', heldFor('rec-3')]]) });

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

	it('hides the upgrade button when the track meets the cutoff', async () => {
		expect.assertions(1);
		auth.role = 'admin';
		await renderList(); // default fixtures: below_cutoff false everywhere

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

	it('keeps the track request usable with a null edition', async () => {
		downloadMutations.requestMutate.mockClear();
		await renderList();

		await page.getByRole('button', { name: 'Request this track' }).click();
		expect(downloadMutations.requestMutate).toHaveBeenCalledTimes(1);
		expect(downloadMutations.requestMutate.mock.calls[0][0].release_id).toBeNull();
	});
});

describe('AlbumTrackList per-track file sizes', () => {
	const TWO_DISCS: AlbumTracksInfo['tracks'] = [
		{
			position: 1,
			disc_number: 1,
			title: 'Disc One Opener',
			length: 100000,
			recording_id: 'rec-d1'
		},
		{
			position: 2,
			disc_number: 1,
			title: 'Disc One Second',
			length: 100000,
			recording_id: 'rec-d2'
		},
		{
			position: 1,
			disc_number: 2,
			title: 'Disc Two Opener',
			length: 100000,
			recording_id: 'rec-d3'
		}
	];

	function localTrack(
		track_number: number,
		disc_number: number,
		size_bytes: number
	): LocalTrackInfo {
		return {
			track_file_id: `f-${disc_number}-${track_number}`,
			title: 'x',
			track_number,
			disc_number,
			size_bytes,
			format: 'flac'
		};
	}

	it('shows formatBytes sizes for matched rows across discs (disc-aware join)', async () => {
		expect.assertions(3);
		await renderList({
			tracks: TWO_DISCS,
			localTracks: [localTrack(1, 1, 10485760), localTrack(1, 2, 15728640)]
		});

		// same position on different discs resolves to different sizes (not joined by title)
		await expect.element(page.getByText('10 MB')).toBeVisible();
		await expect.element(page.getByText('15 MB')).toBeVisible();
		// the unmatched middle row renders the absence marker
		expect(page.getByText('—', { exact: true }).elements()).toHaveLength(1);
	});

	it('shows the absence marker for zero-byte and unmatched rows', async () => {
		expect.assertions(2);
		await renderList({ tracks: TWO_DISCS, localTracks: [localTrack(1, 1, 0)] });

		await expect.element(page.getByText('Disc Two Opener')).toBeVisible();
		expect(page.getByText('—', { exact: true }).elements()).toHaveLength(3);
	});
});

describe('AlbumTrackList 3-dot menu (provider pin)', () => {
	const MENU_TRACKS: AlbumTracksInfo['tracks'] = [
		{ position: 1, disc_number: 1, title: 'First', length: 100000, recording_id: 'rec-m1' },
		{ position: 2, disc_number: 1, title: 'Second', length: 100000, recording_id: 'rec-m2' }
	];

	function menuLocalTrack(n: number): LocalTrackInfo {
		return {
			track_file_id: `menu-file-${n}`,
			title: 'x',
			track_number: n,
			disc_number: 1,
			size_bytes: 1000,
			format: 'flac'
		};
	}

	async function renderMenu(localTracks: LocalTrackInfo[]) {
		closeAllMenus();
		blob.download.mockReset();
		blob.download.mockResolvedValue(undefined);
		player.addToQueue.mockClear();
		// empty library maps: rows show Request, which renders the actions
		// block (and its trigger) the way enabled sources do on a real page
		await renderList({
			useRealMenuItems: true,
			tracks: MENU_TRACKS,
			localTracks,
			byRecording: new Map(),
			byPosition: new Map()
		});
	}

	it('shows a trigger on every row opening the 4-item menu with a working Download', async () => {
		expect.assertions(7);
		await renderMenu([menuLocalTrack(1), menuLocalTrack(2)]);

		await expect.element(page.getByText('First')).toBeVisible();
		const triggers = await page.getByLabelText('More actions').all();
		expect(triggers).toHaveLength(2);

		await triggers[0].click();
		for (const label of ['Add to Queue', 'Play Next', 'Add to Playlist', 'Download']) {
			await expect.element(page.getByRole('menuitem', { name: label })).toBeVisible();
		}
		await page.getByRole('menuitem', { name: 'Download' }).click();
		expect(blob.download).toHaveBeenCalledWith('/api/v1/download/local/track/menu-file-1');
	});

	it('queues through the shared builder', async () => {
		expect.assertions(3);
		await renderMenu([menuLocalTrack(1)]);

		await expect.element(page.getByText('First')).toBeVisible();
		await (await page.getByLabelText('More actions').all())[0].click();
		await page.getByRole('menuitem', { name: 'Add to Queue' }).click();
		expect(player.addToQueue).toHaveBeenCalledTimes(1);
		expect(player.addToQueue.mock.calls[0][0]).toMatchObject({ trackSourceId: 'menu-file-1' });
	});

	it('keeps a single menu open and closes on outside click', async () => {
		expect.assertions(4);
		await renderMenu([menuLocalTrack(1), menuLocalTrack(2)]);

		await expect.element(page.getByText('First')).toBeVisible();
		const triggers = await page.getByLabelText('More actions').all();
		await triggers[0].click();
		await expect.element(page.getByRole('menu')).toBeVisible();

		await triggers[1].click();
		expect(page.getByRole('menu').elements()).toHaveLength(1);

		await page.getByText('First').click();
		await expect.element(page.getByRole('menu')).not.toBeInTheDocument();
	});

	it('omits Download when the row has no local file', async () => {
		expect.assertions(3);
		await renderMenu([]);

		await expect.element(page.getByText('First')).toBeVisible();
		await (await page.getByLabelText('More actions').all())[0].click();
		await expect.element(page.getByRole('menuitem', { name: 'Add to Queue' })).toBeVisible();
		expect(page.getByRole('menuitem', { name: 'Download' }).elements()).toHaveLength(0);
	});
});
