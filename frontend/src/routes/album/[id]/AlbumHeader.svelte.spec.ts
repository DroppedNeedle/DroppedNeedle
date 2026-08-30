import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type {
	AlbumBasicInfo,
	AlbumEditionsResponse,
	AlbumTracksInfo,
	LibraryAlbumDetail,
	LibraryAlbumSummary
} from '$lib/types';

const h = vi.hoisted(() => ({
	editions: undefined as AlbumEditionsResponse | undefined,
	setPin: vi.fn(),
	clearPin: vi.fn(),
	acquire: vi.fn()
}));

vi.mock('$lib/queries/albums/EditionQueries.svelte', () => ({
	getAlbumEditionsQuery: () => ({
		get data() {
			return h.editions;
		}
	}),
	setEditionPin: () => ({ mutateAsync: h.setPin, isPending: false }),
	clearEditionPin: () => ({ mutateAsync: h.clearPin, isPending: false }),
	acquireEdition: () => ({ mutateAsync: h.acquire, isPending: false })
}));

vi.mock('$lib/queries/library/LibraryMutations.svelte', () => ({
	rescanAlbum: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

const localAlbum: LibraryAlbumDetail = {
	id: 'local-album-1',
	title: 'Avalon',
	artist_name: 'Anthony Green',
	artist_id: 'local-artist-1',
	musicbrainz_release_group_id: '4b6276da-e7c7-36df-8771-34b92f774d3b',
	musicbrainz_release_id: '0687c8a5-40a2-4a0c-bdc9-c1d80d94bef5',
	musicbrainz_artist_id: 'eba4c290-2ce6-42c9-affd-5b1ffab84a8f',
	album_identity_state: 'release_linked',
	track_count: 20,
	total_duration_seconds: 3900,
	total_size_bytes: 1,
	format: 'flac',
	year: 2008,
	is_compilation: false,
	cover_available: true,
	date_added: 1,
	sort_name: null,
	original_release_date: '2008-08-04',
	contribution_id: null,
	contribution_state: null,
	row_revision: 4,
	input_revision: 'input-4',
	identification_status: 'identified',
	review_id: null,
	review_revision: null,
	management_identity_readiness: 'ready',
	mapped_track_count: 20,
	management_identity_kind: 'exact_release',
	custom_manifest_id: null,
	custom_manifest_version: null,
	custom_manifest_track_count: 0,
	custom_manifest_recognized_track_count: 0,
	custom_manifest_stale: false,
	management_excluded: false,
	management_exclusion_revision: null,
	management_excluded_at: null,
	active_edition_conversion: null
};

vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryAlbumDetailQuery: () => ({
		data: localAlbum,
		isLoading: false,
		isError: false
	})
}));

vi.mock('$lib/queries/library/LibraryOperationQueries.svelte', () => ({
	getLibraryOperationQuery: () => ({ data: undefined, isError: false })
}));

vi.mock('$lib/queries/library/LibraryEditionQueries.svelte', () => ({
	getReleaseEditionSearchQuery: () => ({
		data: undefined,
		isLoading: false,
		isFetching: false,
		isError: false,
		refetch: vi.fn()
	})
}));

vi.mock('$lib/queries/library/LibraryCatalogMutations.svelte', () => ({
	reidentifyLibraryAlbum: () => ({ mutateAsync: vi.fn(), isPending: false, isError: false }),
	reenableAlbumManagement: () => ({ mutateAsync: vi.fn(), isPending: false, isError: false }),
	selectReidentificationCandidate: () => ({
		mutateAsync: vi.fn(),
		isPending: false,
		isError: false
	})
}));

vi.mock('$lib/queries/library/EditionConversionQueries.svelte', () => {
	const mutation = () => ({ mutateAsync: vi.fn(), isPending: false, reset: vi.fn() });
	return {
		getEditionConversionQuery: () => ({ data: undefined, refetch: vi.fn() }),
		createEditionConversionPreflight: mutation,
		createEditionConversionPreview: mutation,
		startEditionConversion: mutation,
		retryEditionConversion: mutation,
		recheckEditionConversion: mutation,
		cancelEditionConversion: mutation
	};
});

vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	controlLibraryOperation: () => ({ mutateAsync: vi.fn() })
}));

vi.mock('$lib/queries/downloads/UpgradeQueries.svelte', () => ({
	requestUpgradeAlbum: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, isTrusted: true },
	LAST_USER_ID_KEY: 'test:last-user'
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

vi.mock('$lib/stores/deckSampler.svelte', () => ({
	deckSampler: { activeKey: null, status: 'idle', start: vi.fn(), stop: vi.fn() }
}));

const { emptyComponent } = vi.hoisted(() => ({
	emptyComponent: () => {
		const Comp = function () {};
		Comp.prototype = {};
		return { default: Comp };
	}
}));
vi.mock('$lib/components/AlbumImage.svelte', emptyComponent);
vi.mock('$lib/components/HeroBackdrop.svelte', emptyComponent);
vi.mock('$lib/components/downloads/AlbumDownloadStatus.svelte', emptyComponent);

import AlbumHeader from './AlbumHeader.svelte';

const album: AlbumBasicInfo = {
	title: 'Avalon',
	musicbrainz_id: '4b6276da-e7c7-36df-8771-34b92f774d3b',
	artist_name: 'Juliet',
	artist_id: 'artist-1',
	year: 2008,
	in_library: true
};

const tracksInfo: AlbumTracksInfo = {
	tracks: [],
	total_tracks: 20,
	selected_release_mbid: 'release-20'
};

function renderHeader({
	onrefresh = vi.fn(),
	libraryTrackCount = 20,
	libraryBelowCutoff = false,
	localCopies = [],
	trackData = tracksInfo,
	loadingTracks = false
}: {
	onrefresh?: () => void;
	libraryTrackCount?: number;
	libraryBelowCutoff?: boolean;
	localCopies?: LibraryAlbumSummary[];
	trackData?: AlbumTracksInfo;
	loadingTracks?: boolean;
} = {}) {
	render(AlbumHeader, {
		album,
		tracksInfo: trackData,
		loadingTracks,
		inLibrary: true,
		isRequested: false,
		requesting: false,
		refreshing: false,
		headerDownloadTask: null,
		downloadClientConfigured: true,
		libraryInLibrary: true,
		libraryTrackCount,
		libraryBelowCutoff,
		localCopies,
		mbTrackCount: 20,
		releaseGroupMbid: album.musicbrainz_id,
		onrequest: vi.fn(),
		ondelete: vi.fn(),
		onrefresh,
		onartistclick: vi.fn()
	});
	return onrefresh;
}

describe('AlbumHeader automatic edition selection', () => {
	beforeEach(() => {
		h.setPin.mockReset().mockResolvedValue(undefined);
		h.clearPin.mockReset().mockResolvedValue(undefined);
		h.acquire.mockReset().mockResolvedValue({
			release_mbid: 'release-20',
			total_tracks: 20,
			requested: 0,
			upgrades: 0,
			skipped: 20
		});
		h.editions = {
			items: [
				{
					release_mbid: 'release-11',
					track_count: 11,
					title: 'Avalon',
					disambiguation: null,
					date: '2008-08-04',
					country: 'XW',
					packaging: null,
					status: 'Official',
					is_owned: false,
					is_pinned: false
				},
				{
					release_mbid: 'release-20',
					track_count: 20,
					title: 'Avalon',
					disambiguation: null,
					date: '2008-08-05',
					country: 'US',
					packaging: null,
					status: 'Official',
					is_owned: false,
					is_pinned: false
				}
			],
			pinned_release_mbid: null,
			owned_release_mbid: null,
			selected_release_mbid: 'release-11'
		};
	});

	it('shows the refreshed automatic match and refreshes after a manual pin', async () => {
		const onrefresh = renderHeader();
		const trigger = page.getByRole('button', {
			name: 'Edition: Automatic · 2008 · US · 20 tracks'
		});

		await expect.element(trigger).toBeVisible();
		await expect.element(page.getByText('Edition complete', { exact: true })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Acquire this edition' }))
			.not.toBeInTheDocument();
		await trigger.click();
		await expect.element(page.getByText('automatic', { exact: true })).toBeVisible();

		await page.getByRole('button', { name: '2008 · XW · 11 tracks' }).click();
		await vi.waitFor(() => {
			expect(h.setPin).toHaveBeenCalledWith({
				mbid: album.musicbrainz_id,
				releaseMbid: 'release-11'
			});
			expect(onrefresh).toHaveBeenCalledOnce();
		});
	});

	it('uses edition resolution when native tracks have no stored release identity', async () => {
		const editions = h.editions;
		expect(editions).toBeDefined();
		if (!editions) throw new Error('Expected edition data');
		h.editions = { ...editions, selected_release_mbid: 'release-20' };
		renderHeader({
			trackData: { ...tracksInfo, selected_release_mbid: null }
		});

		await expect
			.element(
				page.getByRole('button', {
					name: 'Edition: Automatic · 2008 · US · 20 tracks'
				})
			)
			.toBeVisible();
	});

	it('offers to complete a partial edition', async () => {
		renderHeader({ libraryTrackCount: 11 });

		await expect.element(page.getByRole('button', { name: 'Complete this edition' })).toBeVisible();
		await expect
			.element(page.getByText('Edition complete', { exact: true }))
			.not.toBeInTheDocument();
	});

	it('offers to upgrade a complete edition below the cutoff', async () => {
		renderHeader({ libraryBelowCutoff: true });

		await expect.element(page.getByRole('button', { name: 'Upgrade this edition' })).toBeVisible();
	});

	it('ST7 W2: keeps the picker hidden while tracks load even though the query warms', async () => {
		renderHeader({ loadingTracks: true });
		await expect.element(page.getByRole('button', { name: /Edition: / })).not.toBeInTheDocument();

		// tracks resolve -> picker appears with editions data that warmed earlier
		renderHeader({ loadingTracks: false });
		await expect
			.element(page.getByRole('button', { name: 'Edition: Automatic · 2008 · US · 20 tracks' }))
			.toBeVisible();
	});

	it('exposes local re-identification beside the canonical release controls', async () => {
		renderHeader({ localCopies: [localAlbum] });

		const trigger = page.getByRole('button', { name: 'Re-identify…' });
		await expect.element(trigger).toBeVisible();
		await trigger.click();
		await expect.element(page.getByRole('heading', { name: 'Identify Avalon' })).toBeVisible();
		await expect.element(page.getByText('This screen never writes music files.')).toBeVisible();
	});
});
