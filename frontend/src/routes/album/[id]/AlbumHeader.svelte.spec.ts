import { page } from 'vitest/browser';
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
	acquire: vi.fn()
}));

vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/album/x') } }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

vi.mock('$lib/queries/albums/EditionQueries.svelte', () => ({
	getAlbumEditionsQuery: () => ({
		get data() {
			return h.editions;
		}
	}),
	acquireEdition: () => ({ mutateAsync: h.acquire, isPending: false }),
	getEditionFetchQuery: () => ({ data: { latest: null } }),
	editionFetchLive: () => false,
	getAlbumEditionStatusQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getEditionTracksQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	chooseAlbumEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	handBackAlbumEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	confirmAlbumEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	undoAlbumEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	sealCustomEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	leaveAlbumUnmanaged: () => ({ mutateAsync: vi.fn(), isPending: false }),
	previewEditionTags: () => ({ mutateAsync: vi.fn(), isPending: false }),
	getTagWriteQuery: () => ({ data: undefined, refetch: vi.fn() }),
	tagWriteFinished: vi.fn(),
	writeEditionTags: () => ({ mutateAsync: vi.fn(), isPending: false }),
	undoTagWrite: () => ({ mutateAsync: vi.fn(), isPending: false }),
	editionsKey: (id: string) => ['editions', id],
	getEditionPlanQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	removeEditionExtras: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false }),
	matchEditionFile: () => ({ mutateAsync: vi.fn(), isPending: false }),
	unmatchEditionFile: () => ({ mutateAsync: vi.fn(), isPending: false }),
	getWaitingAlbumsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	invalidateAlbumEdition: () => Promise.resolve()
}));

vi.mock('$lib/queries/library/LibraryMutations.svelte', () => ({
	rescanAlbum: () => ({ mutateAsync: vi.fn(), isPending: false }),
	removeLibraryAlbum: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false }),
	removeLibraryTrack: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false })
}));

const localAlbum: LibraryAlbumDetail = {
	id: 'local-album-1',
	title: 'Glass Harbor',
	artist_name: 'Northern Static',
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
	release_type: 'album',
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
	active_edition_conversion: null,
	display_release_mbid: null,
	pick_basis: null
};

vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryAlbumDetailQuery: () => ({
		data: localAlbum,
		isLoading: false,
		isError: false
	}),
	getLibraryMembershipQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryAlbumsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistsInfiniteQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistThumbsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryStatsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRecentlyAddedQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryAlbumCopiesQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryAlbumTracksQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistDetailQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistAlbumsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistAppearancesQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryScanScheduleQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryAlbumStatusQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibrarySearchQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getAlbumSearchQuery: () => ({ data: undefined, isLoading: false, isError: false })
}));

vi.mock('$lib/queries/library/LibraryOperationQueries.svelte', () => ({
	getCurrentLibraryRunsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRunQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRunHistoryQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRunFailuresQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRunEstimateQuery: () => ({ data: undefined, isLoading: false, isError: false })
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
	reenableAlbumManagement: () => ({ mutateAsync: vi.fn(), isPending: false, isError: false })
}));

vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	controlLibraryOperation: () => ({ mutateAsync: vi.fn() }),
	controlLibraryRun: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false }),
	controlIdentification: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/library-management/LibraryManagementMutations.svelte', () => ({
	handBackLibraryAlbumMutation: () => ({ mutateAsync: vi.fn(), isPending: false, error: null }),
	handBackRefusalMessage: () => null
}));

vi.mock('$lib/queries/downloads/UpgradeQueries.svelte', () => ({
	requestUpgradeAlbum: () => ({ mutateAsync: vi.fn(), isPending: false }),
	getCutoffUnmetQuery: () => ({ data: undefined, isLoading: false, isError: false })
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

const blob = vi.hoisted(() => ({ download: vi.fn() }));
vi.mock('$lib/utils/blobDownload', () => ({ downloadBlob: blob.download }));

import AlbumHeader from './AlbumHeader.svelte';

const album: AlbumBasicInfo = {
	title: 'Glass Harbor',
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

async function renderHeader({
	onrefresh = vi.fn(),
	libraryTrackCount = 20,
	libraryBelowCutoff = false,
	localCopies = [],
	trackData = tracksInfo,
	loadingTracks = false,
	downloadAllowed = true
}: {
	onrefresh?: () => void;
	libraryTrackCount?: number;
	libraryBelowCutoff?: boolean;
	localCopies?: LibraryAlbumSummary[];
	trackData?: AlbumTracksInfo;
	loadingTracks?: boolean;
	downloadAllowed?: boolean;
} = {}) {
	await render(AlbumHeader, {
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
		downloadAllowed,
		mbTrackCount: 20,
		releaseGroupMbid: album.musicbrainz_id,
		onrequest: vi.fn(),
		ondelete: vi.fn(),
		onrefresh,
		onartistclick: vi.fn()
	});
	return onrefresh;
}

describe('AlbumHeader album download button', () => {
	beforeEach(() => {
		blob.download.mockReset();
		blob.download.mockResolvedValue(undefined);
	});

	it('renders nothing when downloads are restricted', async () => {
		expect.assertions(2);
		await renderHeader({ localCopies: [localAlbum], downloadAllowed: false });

		await expect.element(page.getByRole('heading', { name: 'Glass Harbor' })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Download album/ }))
			.not.toBeInTheDocument();
	});
});
