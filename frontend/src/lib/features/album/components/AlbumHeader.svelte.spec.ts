import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type {
	AlbumBasicInfo,
	AlbumEditionsResponse,
	AlbumTracksInfo,
	AlbumDetailView,
	AlbumView
} from '$lib/types';

const h = vi.hoisted(() => ({
	editions: undefined as AlbumEditionsResponse | undefined,
	acquire: vi.fn()
}));

vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/album/x') } }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

vi.mock('$lib/features/library/queries/EditionQueries.svelte', () => ({
	getAlbumEditionsQuery: () => ({
		get data() {
			return h.editions;
		}
	}),
	createAcquireEditionMutation: () => ({ mutateAsync: h.acquire, isPending: false }),
	getEditionFetchQuery: () => ({ data: { latest: null } }),
	editionFetchLive: () => false,
	getAlbumEditionStatusQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getEditionTracksQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	createChooseAlbumEditionMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createHandBackAlbumEditionMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createConfirmAlbumEditionMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createMarkLooksWrongMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createClearLooksWrongMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createUndoAlbumEditionMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createSealCustomEditionMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createLeaveAlbumUnmanagedMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createPreviewEditionTagsMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	getTagWriteQuery: () => ({ data: undefined, refetch: vi.fn() }),
	createWriteEditionTagsMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createUndoTagWriteMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	editionsKey: (id: string) => ['editions', id],
	getEditionPlanQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	createRemoveEditionExtrasMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	createMatchEditionFileMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createUnmatchEditionFileMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	getWaitingAlbumsQuery: () => ({ data: undefined, isLoading: false, isError: false })
}));

vi.mock('$lib/features/library/queries/LibraryMutations.svelte', () => ({
	createRescanAlbumMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createRemoveLibraryAlbumMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	createRemoveLibraryTrackMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	})
}));

const localAlbum: AlbumDetailView = {
	id: 'local-album-1',
	title: 'Glass Harbor',
	artist_name: 'Northern Static',
	artist_id: 'local-artist-1',
	release_group_mbid: '4b6276da-e7c7-36df-8771-34b92f774d3b',
	release_mbid: '0687c8a5-40a2-4a0c-bdc9-c1d80d94bef5',
	artist_mbid: 'eba4c290-2ce6-42c9-affd-5b1ffab84a8f',
	identity_state: 'linked',
	track_count: 20,
	total_duration_seconds: 3900,
	total_size_bytes: 1,
	format: 'flac',
	year: 2008,
	is_compilation: false,
	cover_available: true,
	art_missing: false,
	favorite: false,
	date_added: 1,
	contribution_id: null,
	contribution_state: null,
	row_revision: 4,
	pending_review_id: null
};

vi.mock('$lib/features/library/queries/LibraryQueries.svelte', () => ({
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

vi.mock('$lib/features/library-management/queries/LibraryOperationQueries.svelte', () => ({
	getCurrentLibraryRunsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRunQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRunHistoryQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRunFailuresQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRunEstimateQuery: () => ({ data: undefined, isLoading: false, isError: false })
}));

vi.mock('$lib/features/downloads/queries/HeldQueries.svelte', () => ({
	createImportHeldMutation: () => ({ mutate: vi.fn(), isPending: false })
}));

vi.mock('$lib/features/library/queries/LibraryEditionQueries.svelte', () => ({
	getReleaseEditionSearchQuery: () => ({
		data: undefined,
		isLoading: false,
		isFetching: false,
		isError: false,
		refetch: vi.fn()
	})
}));

vi.mock('$lib/features/library/queries/LibraryCatalogMutations.svelte', () => ({
	createReenableAlbumManagementMutation: () => ({
		mutateAsync: vi.fn(),
		isPending: false,
		isError: false
	})
}));

vi.mock('$lib/features/library-management/queries/LibraryOperationMutations.svelte', () => ({
	createControlLibraryOperationMutation: () => ({ mutateAsync: vi.fn() }),
	createControlLibraryRunMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	createControlIdentificationMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	})
}));

vi.mock('$lib/features/library-management/queries/LibraryManagementMutations.svelte', () => ({
	createHandBackLibraryAlbumMutation: () => ({
		mutateAsync: vi.fn(),
		isPending: false,
		error: null
	}),
	handBackRefusalMessage: () => null
}));

vi.mock('$lib/features/downloads/queries/UpgradeQueries.svelte', () => ({
	createRequestUpgradeAlbumMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	getCutoffUnmetQuery: () => ({ data: undefined, isLoading: false, isError: false })
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, isTrusted: true },
	LAST_USER_ID_KEY: 'test:last-user'
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

vi.mock('$lib/features/discover/state/deckSampler.svelte', () => ({
	deckSampler: { activeKey: null, status: 'idle', start: vi.fn(), stop: vi.fn() }
}));

const { emptyComponent } = vi.hoisted(() => ({
	emptyComponent: () => {
		const Comp = function () {};
		Comp.prototype = {};
		return { default: Comp };
	}
}));
vi.mock('$lib/media/AlbumImage.svelte', emptyComponent);
vi.mock('$lib/media/HeroBackdrop.svelte', emptyComponent);
vi.mock('$lib/features/downloads/components/AlbumDownloadStatus.svelte', emptyComponent);

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
	localCopies?: AlbumView[];
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
