import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

type HydrateOptions = {
	cache: unknown;
	cacheKey?: string;
	onHydrate: (value: unknown) => void;
};

const {
	mockGoto,
	mockPageFetch,
	mockHydrateDetailCacheEntry,
	mockAlbumBasicCache,
	mockAlbumTracksCache,
	mockAlbumDiscoveryCache,
	mockAlbumLastFmCache,
	mockAlbumYouTubeCache,
	mockAlbumSourceMatchCache,
	mockDownloadsData,
	mockHeldData,
	mockLibraryStatusData,
	mockLocalCopiesData,
	mockLocalDetailData,
	mockEditionsData,
	mockEditionStatus,
	mockEditionPlan,
	mockHistoryData,
	mockDetailIdsRead,
	mockLocalMatch,
	mockNavidromeMatch
} = vi.hoisted(() => ({
	mockLocalMatch: vi.fn(),
	mockNavidromeMatch: vi.fn(),
	mockGoto: vi.fn(),
	mockPageFetch: vi.fn(),
	mockHydrateDetailCacheEntry: vi.fn(),
	mockAlbumBasicCache: { set: vi.fn() },
	mockAlbumTracksCache: { set: vi.fn() },
	mockAlbumDiscoveryCache: { set: vi.fn() },
	mockAlbumLastFmCache: { set: vi.fn() },
	mockAlbumYouTubeCache: { get: vi.fn(), set: vi.fn() },
	mockAlbumSourceMatchCache: {
		get: vi.fn(),
		set: vi.fn(),
		remove: vi.fn(),
		isStale: vi.fn(() => false)
	},
	mockDownloadsData: { value: undefined as unknown },
	mockHeldData: { value: { items: [] } as unknown },
	mockLibraryStatusData: { value: undefined as unknown },
	mockLocalCopiesData: { value: {} as Record<string, unknown> },
	mockLocalDetailData: { value: {} as Record<string, unknown> },
	mockEditionsData: { value: undefined as unknown },
	mockEditionStatus: { value: undefined as unknown },
	mockEditionPlan: { value: undefined as unknown },
	mockHistoryData: { value: undefined as unknown },
	mockDetailIdsRead: new Set<string>()
}));

vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/album/x') } }));
vi.mock('$app/navigation', () => ({
	goto: (...args: unknown[]) => mockGoto(...args)
}));

vi.mock('$lib/stores/library', () => ({
	libraryStore: {
		isInLibrary: vi.fn(() => false),
		isRequested: vi.fn(() => false)
	}
}));

const { mockRemoveTrack } = vi.hoisted(() => ({
	mockRemoveTrack: { mutate: vi.fn(), isPending: false }
}));

const integrationState = {
	youtube: false,
	youtube_api: false,
	jellyfin: false,
	localfiles: true,
	navidrome: true,
	lastfm: false,
	download_client: true,
	library: true
};

vi.mock('$lib/stores/integration', () => ({
	integrationStore: {
		subscribe: vi.fn((cb: (value: unknown) => void) => {
			cb(integrationState);
			return () => {};
		}),
		ensureLoaded: vi.fn().mockResolvedValue(undefined)
	}
}));

vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		isPlaying: false,
		nowPlaying: null,
		currentQueueItem: null,
		addToQueue: vi.fn(),
		playNext: vi.fn(),
		playMultipleNext: vi.fn(),
		addMultipleToQueue: vi.fn()
	}
}));

vi.mock('$lib/utils/navigationAbort', () => ({
	pageFetch: (...args: unknown[]) => mockPageFetch(...args)
}));

vi.mock('$lib/utils/detailCacheHydration', () => ({
	hydrateDetailCacheEntry: (...args: unknown[]) => mockHydrateDetailCacheEntry(...args)
}));

vi.mock('$lib/utils/albumDetailCache', () => ({
	albumBasicCache: mockAlbumBasicCache,
	albumTracksCache: mockAlbumTracksCache,
	albumDiscoveryCache: mockAlbumDiscoveryCache,
	albumLastFmCache: mockAlbumLastFmCache,
	albumYouTubeCache: mockAlbumYouTubeCache,
	albumSourceMatchCache: mockAlbumSourceMatchCache,
	albumSourceMatchCacheKey: (id: string) => `key:${id}`,
	clearMusicBrainzProviderCaches: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	})
}));

vi.mock('$lib/utils/serviceStatus', () => ({
	extractServiceStatus: vi.fn()
}));

// Stub the library status query so the page renders without a QueryClientProvider.
// Source matches come from the typed v3 reads; stub them at the module edge.
vi.mock('$lib/queries/albumMatch', () => ({
	fetchLocalAlbumMatch: (...args: unknown[]) => mockLocalMatch(...args),
	fetchNavidromeAlbumMatch: (...args: unknown[]) => mockNavidromeMatch(...args),
	fetchJellyfinAlbumMatch: () =>
		Promise.resolve({ found: false, jellyfin_album_id: null, tracks: [] }),
	fetchPlexAlbumMatch: () => Promise.resolve({ found: false, plex_album_id: null, tracks: [] })
}));

vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryAlbumStatusQuery: () => ({
		get data() {
			return mockLibraryStatusData.value;
		},
		isError: false,
		refetch: vi.fn()
	}),
	cacheCanonicalLibraryAlbumDetail: vi.fn(),
	// Keyed by the asked id: these reads only answer local album ids.
	getLibraryAlbumCopiesQuery: (getAlbumId: () => string) => ({
		get data() {
			return mockLocalCopiesData.value[getAlbumId()];
		},
		isLoading: false
	}),
	// Records each id it is asked to read, as the real query would fetch it.
	getLibraryAlbumDetailQuery: (getAlbumId: () => string) => ({
		get data() {
			const id = getAlbumId();
			if (id) mockDetailIdsRead.add(id);
			return mockLocalDetailData.value[id];
		},
		isLoading: false,
		isError: false
	}),
	// ProviderAlbumPage now statically imports LocalAlbumPage →
	// LocalAlbumTrackList + AlbumOrganizationDialog, which need these at
	// import time.
	getLibraryAlbumTracksQuery: () => ({
		data: { items: [], total: 0 },
		isLoading: false
	}),
	getLibraryAlbumsQuery: () => ({ data: { items: [] }, isLoading: false }),
	getLibraryMembershipQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistsInfiniteQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistThumbsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryStatsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRecentlyAddedQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistDetailQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistAlbumsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistAppearancesQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryScanScheduleQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibrarySearchQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getAlbumSearchQuery: () => ({ data: undefined, isLoading: false, isError: false })
}));

// Where-to-buy section (Get it): stub so the page renders without a QueryClientProvider
vi.mock('$lib/queries/albums/GetItQueries.svelte', () => ({
	getPurchaseOptionsQuery: () => ({ data: undefined, isLoading: false }),
	getArtistPurchaseOptionsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getAlbumPluginDetailsQuery: () => ({ data: undefined, isLoading: false, isError: false })
}));

// album-scoped downloads query: feed it per-test via mockDownloadsData so the page renders without
// a QueryClientProvider (same approach as the library status query above)
vi.mock('$lib/queries/downloads/DownloadQueries.svelte', () => ({
	getAlbumDownloadsQuery: () => ({
		get data() {
			return mockDownloadsData.value;
		},
		refetch: vi.fn()
	}),
	getDownloadActivitySummaryQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getDownloadsQuery: () => ({ data: undefined, isLoading: false, isError: false })
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	tryNextSource: () => ({ mutate: vi.fn(), isPending: false }),
	cancelDownload: () => ({ mutate: vi.fn(), isPending: false }),
	retryDownload: () => ({ mutate: vi.fn(), isPending: false }),
	stopAutoRetry: () => ({ mutate: vi.fn(), isPending: false }),
	requestTrack: () => ({ mutate: vi.fn(), isPending: false }),
	importHeldTrack: () => ({ mutate: vi.fn(), isPending: false }),
	discardHeldTrack: () => ({ mutate: vi.fn(), isPending: false }),
	reverifyHeldTrack: () => ({ mutate: vi.fn(), isPending: false }),
	requestAlbum: () => ({
		mutateAsync: vi.fn().mockResolvedValue({ success: true }),
		isPending: false
	}),
	clearFinished: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/albums/EditionQueries.svelte', () => ({
	getAlbumEditionsQuery: () => ({
		get data() {
			return mockEditionsData.value;
		}
	}),
	acquireEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	getAlbumEditionStatusQuery: () => ({
		get data() {
			return mockEditionStatus.value;
		},
		isPending: false,
		isError: false
	}),
	chooseAlbumEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	handBackAlbumEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	confirmAlbumEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	undoAlbumEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	sealCustomEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	leaveAlbumUnmanaged: () => ({ mutateAsync: vi.fn(), isPending: false }),
	previewEditionTags: () => ({ mutateAsync: vi.fn(), isPending: false }),
	writeEditionTags: () => ({ mutateAsync: vi.fn(), isPending: false }),
	undoTagWrite: () => ({ mutateAsync: vi.fn(), isPending: false }),
	editionsKey: (id: string) => ['editions', id],
	getEditionPlanQuery: () => ({
		get data() {
			return mockEditionPlan.value;
		},
		isLoading: false,
		isError: false
	}),
	removeEditionExtras: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false }),
	restoreEditionExtras: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false }),
	matchEditionFile: () => ({ mutateAsync: vi.fn(), isPending: false }),
	unmatchEditionFile: () => ({ mutateAsync: vi.fn(), isPending: false }),
	getWaitingAlbumsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	invalidateAlbumEdition: () => Promise.resolve()
}));

vi.mock('$lib/queries/library/LibraryAlbumHistoryQueries.svelte', () => ({
	getAlbumHistoryQuery: () => ({
		get data() {
			return mockHistoryData.value;
		},
		isPending: false,
		isError: false,
		refetch: vi.fn()
	})
}));

vi.mock('$lib/queries/library/LibraryRetagPairQueries.svelte', () => ({
	getRetagPairsQuery: () => ({ data: { pairs: [], total: 0 } }),
	joinRetagPair: () => ({ mutateAsync: vi.fn(), isPending: false }),
	dismissRetagPair: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/downloads/UpgradeQueries.svelte', () => ({
	getCutoffUnmetQuery: () => ({
		get data() {
			return undefined;
		}
	}),
	requestUpgradeAlbum: () => ({ mutateAsync: vi.fn(), isPending: false }),
	requestUpgradeTrack: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/downloads/HeldQueries.svelte', () => ({
	getHeldImportsQuery: () => ({
		get data() {
			return mockHeldData.value;
		}
	})
}));

vi.mock('$lib/queries/downloads/DownloadSSE.svelte', () => ({
	createDownloadStream: () => ({
		state: { progress: null, status: null, source: null, done: false },
		start: vi.fn(),
		stop: vi.fn()
	})
}));

// Stub only the rescan mutation factory so AlbumHeader renders without a QueryClientProvider; keep other exports intact.
vi.mock('$lib/queries/library/LibraryMutations.svelte', () => ({
	rescanAlbum: () => ({ mutateAsync: vi.fn(), isPending: false }),
	// the orphan-review section (P5) creates its removal mutation at init - stub it
	// so the page renders without a QueryClientProvider
	removeLibraryTrack: () => mockRemoveTrack,
	removeLibraryAlbum: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false })
}));
vi.mock('$lib/components/AlbumImage.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/Toast.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/DiscoveryAlbumCarousel.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/LastFmAlbumEnrichment.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/AddToPlaylistModal.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/BackButton.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/TrackPlayButton.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/JellyfinIcon.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/LocalFilesIcon.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/NavidromeIcon.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/DeleteAlbumModal.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/NowPlayingIndicator.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
// The left-unmanaged section reads its own queries; its presence is not what these specs check.
vi.mock('$lib/components/library/ManagementHoldSection.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});

vi.mock('$lib/player/launchJellyfinPlayback', () => ({ launchJellyfinPlayback: vi.fn() }));
vi.mock('$lib/player/launchLocalPlayback', () => ({ launchLocalPlayback: vi.fn() }));
vi.mock('$lib/player/launchNavidromePlayback', () => ({ launchNavidromePlayback: vi.fn() }));

import AlbumPage from './ProviderAlbumPage.svelte';
import AlbumRoute from './+page.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

const albumId = '3f3a6d95-326e-4384-80b0-0744f20f24ff';

function jsonResponse(payload: unknown, status = 200): Response {
	return new Response(JSON.stringify(payload), {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
}

describe('album detail page track rendering', () => {
	beforeEach(() => {
		mockGoto.mockReset();
		mockPageFetch.mockReset();
		mockHydrateDetailCacheEntry.mockReset();
		mockDownloadsData.value = undefined;
		mockHeldData.value = { items: [] };
		mockLibraryStatusData.value = undefined;
		mockLocalCopiesData.value = {};
		mockLocalDetailData.value = {};
		mockEditionsData.value = undefined;
		mockEditionStatus.value = undefined;
		mockEditionPlan.value = undefined;
		mockHistoryData.value = undefined;
		mockDetailIdsRead.clear();
		mockHydrateDetailCacheEntry.mockImplementation(({ cache, onHydrate }: HydrateOptions) => {
			if (cache === mockAlbumBasicCache) {
				onHydrate({
					title: 'Daylight Hours',
					musicbrainz_id: albumId,
					artist_name: 'Lumen',
					artist_id: 'artist-1',
					in_library: true,
					requested: false,
					cover_url: null
				});
				return false;
			}

			if (cache === mockAlbumTracksCache) {
				onHydrate({
					tracks: [
						{
							position: 1,
							disc_number: 1,
							title: 'Infinite ❤️ Without Fulfillment',
							length: 95327
						},
						{ position: 5, disc_number: 1, title: 'Circumambient', length: 223280 },
						{ position: 1, disc_number: 2, title: 'Ambrosia', length: 213093 },
						{ position: 5, disc_number: 2, title: 'Be a Body (Baarsden rework)', length: 204626 }
					],
					total_tracks: 4,
					total_length: 736326,
					label: null,
					barcode: null,
					country: null,
					selected_release_mbid: 'release-remaster'
				});
				return false;
			}

			if (cache === mockAlbumDiscoveryCache) {
				return true;
			}

			if (cache === mockAlbumLastFmCache) {
				return true;
			}

			return true;
		});
		mockLocalMatch.mockReset().mockImplementation((mbid: string) =>
			Promise.resolve(
				mbid === albumId
					? {
							found: true,
							tracks: [
								{
									track_file_id: '1',
									title: 'Infinite ❤️ Without Fulfillment',
									track_number: 1,
									disc_number: 1,
									duration_seconds: 95,
									size_bytes: 1,
									format: 'flac'
								},
								{
									track_file_id: '2',
									title: 'Circumambient',
									track_number: 5,
									disc_number: 1,
									duration_seconds: 223,
									size_bytes: 1,
									format: 'flac'
								},
								{
									track_file_id: '3',
									title: 'Ambrosia',
									track_number: 1,
									disc_number: 2,
									duration_seconds: 213,
									size_bytes: 1,
									format: 'flac'
								},
								{
									track_file_id: '4',
									title: 'Be a Body (Baarsden rework)',
									track_number: 5,
									disc_number: 2,
									duration_seconds: 204,
									size_bytes: 1,
									format: 'flac'
								}
							],
							total_size_bytes: 4,
							primary_format: 'flac'
						}
					: { found: false, musicbrainz_id: mbid, tracks: [], total_size_bytes: 0 }
			)
		);
		mockNavidromeMatch.mockReset().mockResolvedValue({
			found: true,
			navidrome_album_id: 'nav-1',
			tracks: [
				{
					navidrome_id: 'n1',
					title: 'Infinite ❤️ Without Fulfillment',
					track_number: 1,
					disc_number: 1,
					duration_seconds: 95,
					codec: 'flac',
					bitrate: 800,
					album_name: 'Daylight Hours',
					artist_name: 'Lumen'
				},
				{
					navidrome_id: 'n2',
					title: 'Circumambient',
					track_number: 5,
					disc_number: 1,
					duration_seconds: 223,
					codec: 'flac',
					bitrate: 800,
					album_name: 'Daylight Hours',
					artist_name: 'Lumen'
				},
				{
					navidrome_id: 'n3',
					title: 'Ambrosia',
					track_number: 1,
					disc_number: 2,
					duration_seconds: 213,
					codec: 'flac',
					bitrate: 800,
					album_name: 'Daylight Hours',
					artist_name: 'Lumen'
				},
				{
					navidrome_id: 'n4',
					title: 'Be a Body (Baarsden rework)',
					track_number: 5,
					disc_number: 2,
					duration_seconds: 204,
					codec: 'flac',
					bitrate: 800,
					album_name: 'Daylight Hours',
					artist_name: 'Lumen'
				}
			]
		});
		mockPageFetch.mockImplementation((input: string | URL) => {
			const url = typeof input === 'string' ? input : input.toString();

			if (url.endsWith(`/api/v3/albums/${albumId}/basic`)) {
				return Promise.resolve(
					jsonResponse({
						title: 'Daylight Hours',
						musicbrainz_id: albumId,
						artist_name: 'Lumen',
						artist_id: 'artist-1',
						in_library: true,
						requested: false,
						cover_url: null
					})
				);
			}

			if (url.endsWith(`/api/v3/albums/${albumId}/tracks`)) {
				return Promise.resolve(
					jsonResponse({
						tracks: [
							{
								position: 1,
								disc_number: 1,
								title: 'Infinite ❤️ Without Fulfillment',
								length: 95327
							},
							{ position: 5, disc_number: 1, title: 'Circumambient', length: 223280 },
							{ position: 1, disc_number: 2, title: 'Ambrosia', length: 213093 },
							{ position: 5, disc_number: 2, title: 'Be a Body (Baarsden rework)', length: 204626 }
						],
						total_tracks: 4,
						total_length: 736326,
						label: null,
						barcode: null,
						country: null,
						selected_release_mbid: 'release-remaster'
					})
				);
			}

			if (url.includes(`/api/v3/albums/${albumId}/more-by-artist`)) {
				return Promise.resolve(jsonResponse({ artist_name: 'Lumen', albums: [] }));
			}

			if (url.includes(`/api/v3/albums/${albumId}/similar`)) {
				return Promise.resolve(jsonResponse({ albums: [] }));
			}

			if (url.endsWith(`/api/v3/youtube/link/${albumId}`)) {
				return Promise.resolve(new Response(null, { status: 204 }));
			}

			if (url.endsWith(`/api/v3/youtube/track-links/${albumId}`)) {
				return Promise.resolve(jsonResponse([]));
			}

			return Promise.resolve(jsonResponse({}));
		});
	});

	beforeEach(() => {
		mockRemoveTrack.mutate.mockClear();
		mockRemoveTrack.isPending = false;
		authStore.setUser(
			toAuthUser({
				id: 'user-1',
				display_name: 'admin',
				role: 'admin',
				email: null,
				avatar_url: null,
				username: 'admin',
				username_display: 'admin'
			})
		);
	});

	function localMatchFetchCount(): number {
		return mockLocalMatch.mock.calls.length;
	}

	async function openRemoveFileDialog(): Promise<void> {
		const row = page
			.getByRole('listitem')
			.filter({ hasText: 'Infinite ❤️ Without Fulfillment' })
			.first();
		await row.getByLabelText('More actions').click();
		await page.getByRole('menuitem', { name: 'Remove file' }).click();
		await expect.element(page.getByRole('dialog')).toBeVisible();
	}

	it('removes the file with the captured ids and refetches only that album', async () => {
		await render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);
		await openRemoveFileDialog();

		await page.getByRole('dialog').getByRole('button', { name: 'Remove', exact: true }).click();

		expect(mockRemoveTrack.mutate).toHaveBeenCalledTimes(1);
		const [, options] = mockRemoveTrack.mutate.mock.calls[0];

		const before = localMatchFetchCount();
		await options.onSuccess();
		await vi.waitFor(() => expect(localMatchFetchCount()).toBe(before + 1));
		expect(mockLocalMatch.mock.calls.every(([mbid]) => mbid === albumId)).toBe(true);
		await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
	});

	it('cancel closes the dialog without removing the file', async () => {
		await render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);
		await openRemoveFileDialog();

		await page.getByRole('dialog').getByRole('button', { name: 'Cancel' }).click();

		await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
		expect(mockRemoveTrack.mutate).not.toHaveBeenCalled();
	});

	it('surfaces a removal failure in the dialog and refetches nothing', async () => {
		await render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);
		await openRemoveFileDialog();

		await page.getByRole('dialog').getByRole('button', { name: 'Remove', exact: true }).click();
		const [, options] = mockRemoveTrack.mutate.mock.calls[0];
		const before = localMatchFetchCount();
		options.onError();

		await expect.element(page.getByRole('alert')).toHaveTextContent("Couldn't remove this file");
		await expect.element(page.getByRole('dialog')).toBeVisible();
		expect(localMatchFetchCount()).toBe(before);
	});

	it('drops the dialog on navigation and never refetches the old album', async () => {
		const view = await render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);
		await openRemoveFileDialog();
		await page.getByRole('dialog').getByRole('button', { name: 'Remove', exact: true }).click();
		const [, options] = mockRemoveTrack.mutate.mock.calls[0];

		const otherAlbumId = '5b0f0f11-1111-4111-8111-111111111111';
		await view.rerender({
			data: { albumId: otherAlbumId }
		});

		await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
		const oldAlbumReads = () =>
			mockPageFetch.mock.calls.filter(([url]) => String(url).endsWith(albumId)).length +
			mockLocalMatch.mock.calls.filter(([mbid]) => mbid === albumId).length;
		const before = oldAlbumReads();
		await options.onSuccess();
		expect(oldAlbumReads()).toBe(before);
	});

	describe('matched album edition controls', () => {
		const localAlbumId = 'local-album-1';

		function holdLocalCopy(): void {
			mockLibraryStatusData.value = {
				in_library: true,
				album_id: localAlbumId,
				track_count: 0,
				tracks: []
			};
			mockLocalDetailData.value = {
				[localAlbumId]: {
					id: localAlbumId,
					title: 'Daylight Hours',
					artist_name: 'Lumen',
					musicbrainz_release_group_id: albumId
				}
			};
			mockLocalCopiesData.value = { [localAlbumId]: { items: [] } };
			mockEditionStatus.value = {
				album_id: localAlbumId,
				candidates: [],
				edition_kind: 'none',
				files_by_itself: true,
				identifying: false,
				identity_revision: 0,
				placing: null,
				reason: { code: 'UNIDENTIFIED', message: 'Not matched yet.', action: '' },
				release_mbid: null,
				release_group_mbid: null,
				state: 'unidentified',
				undo_available: false,
				writing: false
			};
		}

		async function renderMatchedAlbum(): Promise<void> {
			await render(AlbumPage, {
				props: { data: { albumId } }
			} as Parameters<typeof render<typeof AlbumPage>>[1]);
			await expect
				.element(page.getByRole('heading', { name: 'Daylight Hours', level: 1 }))
				.toBeVisible();
		}

		beforeEach(() => {
			mockEditionsData.value = {
				items: [
					{
						release_mbid: 'release-1',
						disambiguation: null,
						date: '2012-02-21',
						country: 'US',
						track_count: 4,
						is_pinned: false
					}
				],
				selected_release_mbid: 'release-1'
			};
		});

		it('shows the edition plate of the copy the release group holds', async () => {
			holdLocalCopy();
			await renderMatchedAlbum();

			await expect
				.element(page.getByRole('button', { name: /^Edition: No edition yet\./ }))
				.toBeVisible();
		});

		it('shows no edition plate when the library holds no copy', async () => {
			mockLibraryStatusData.value = {
				in_library: false,
				album_id: albumId,
				track_count: 0,
				tracks: []
			};
			await renderMatchedAlbum();

			await expect.element(page.getByRole('button', { name: /^Edition:/ })).not.toBeInTheDocument();
		});
	});

	describe('the page agrees with the edition', () => {
		const localAlbumId = 'local-album-1';
		const songs: [number, number, string, number][] = [
			[1, 1, 'Infinite ❤️ Without Fulfillment', 95327],
			[1, 5, 'Circumambient', 223280],
			[2, 1, 'Ambrosia', 213093],
			[2, 5, 'Be a Body (Baarsden rework)', 204626]
		];

		// After choosing the remaster: Circumambient's file is another version
		// (3:30 against the edition's 3:43), the others sit on their tracks.
		function chooseRemaster(): void {
			mockLibraryStatusData.value = {
				in_library: true,
				album_id: localAlbumId,
				track_count: 4,
				tracks: songs.map(([disc, position, title], i) => ({
					id: `file-${i}`,
					title,
					disc_number: disc,
					track_number: position,
					format: 'flac',
					artist_name: 'Lumen',
					musicbrainz_recording_id: null,
					below_cutoff: false
				}))
			};
			mockLocalDetailData.value = {
				[localAlbumId]: {
					id: localAlbumId,
					title: 'Daylight Hours',
					artist_name: 'Lumen',
					musicbrainz_release_group_id: albumId
				}
			};
			mockLocalCopiesData.value = { [localAlbumId]: { items: [] } };
			mockEditionPlan.value = {
				album_id: localAlbumId,
				release_mbid: 'release-remaster',
				kept_count: 3,
				missing_count: 1,
				other_version_count: 1,
				duplicate_count: 0,
				extra_count: 0,
				files: songs.map(([disc, position, title], i) => ({
					local_track_id: `file-${i}`,
					title,
					disc_number: disc,
					track_number: position,
					edition_disc_number: disc,
					edition_position: position,
					fate: title === 'Circumambient' ? 'other_version' : 'keep',
					length_ms: title === 'Circumambient' ? 210000 : 200000,
					note: null
				})),
				tracks: songs.map(([disc, position, title, length], i) => ({
					disc_number: disc,
					position,
					title,
					length_ms: length,
					local_track_id: title === 'Circumambient' ? null : `file-${i}`,
					recording_mbid: `rec-${i}`,
					release_track_mbid: `track-${i}`
				}))
			};
		}

		function signIn(role: 'admin' | 'user'): void {
			authStore.setUser(
				toAuthUser({
					id: 'user-1',
					display_name: 'Someone',
					role,
					email: null,
					avatar_url: null,
					username: 'someone',
					username_display: 'Someone'
				})
			);
		}

		async function renderPage(): Promise<void> {
			await render(AlbumPage, {
				props: { data: { albumId } }
			} as Parameters<typeof render<typeof AlbumPage>>[1]);
			await expect
				.element(page.getByRole('heading', { name: 'Daylight Hours', level: 1 }))
				.toBeVisible();
		}

		const trackRow = (title: string) =>
			page.getByRole('listitem').filter({ hasText: title }).first();

		for (const role of ['admin', 'user'] as const) {
			it(`shows the chosen edition's counts and gaps to ${role === 'admin' ? 'an admin' : 'a plain user'}`, async () => {
				signIn(role);
				chooseRemaster();
				await renderPage();

				const missing = trackRow('Circumambient');
				await expect.element(missing.getByText('Missing', { exact: true })).toBeVisible();
				await expect.element(missing.getByText('3:43')).toBeVisible();
				await expect.element(missing.getByText('Your version 3:30')).toBeVisible();
				await expect.element(trackRow('Ambrosia').getByText('3:20')).toBeVisible();
				await expect.element(page.getByText('3/4', { exact: true })).toBeVisible();
				await expect.element(page.getByText('Edition complete')).not.toBeInTheDocument();
				await expect
					.element(page.getByText('Another version of track 5', { exact: true }))
					.toBeVisible();
			});
		}

		it("shows a file's note on its row", async () => {
			chooseRemaster();
			const plan = mockEditionPlan.value as {
				files: { title: string; fate: string; note: unknown }[];
			};
			plan.files[2].note = { code: 'TRACK_ID_CORRECTED', message: '', action: '' };
			plan.files.push({
				...(plan.files[0] as object),
				local_track_id: 'file-other',
				title: 'Genesis',
				fate: 'extra',
				edition_position: null,
				note: { code: 'TRACK_ID_ELSEWHERE', message: '', action: '' }
			} as (typeof plan.files)[number]);
			await renderPage();

			await expect
				.element(
					trackRow('Ambrosia').getByText(
						"This file's track id belonged to another edition; it will be corrected when tags are written"
					)
				)
				.toBeVisible();
			const leftOut = page.getByRole('listitem').filter({ hasText: 'Genesis' });
			await expect.element(leftOut.getByText('Not on this edition')).toBeVisible();
			await expect
				.element(leftOut.getByText("This file's track id belongs to another edition of this album"))
				.toBeVisible();
		});

		it('shows the album history below the track list', async () => {
			chooseRemaster();
			mockHistoryData.value = {
				items: [
					{
						kind: 'tracks_continued',
						at: Date.UTC(2026, 9, 12),
						by: null,
						track_count: 1,
						track_titles: ['Ambrosia'],
						other_album_title: null
					}
				]
			};
			await renderPage();

			const history = page.getByRole('heading', { name: 'Album history' });
			await expect.element(history).toBeVisible();
			const tracks = page.getByRole('heading', { name: 'Tracks', exact: true }).element();
			expect(
				tracks.compareDocumentPosition(history.element()) & Node.DOCUMENT_POSITION_FOLLOWING
			).toBeTruthy();
		});

		it('never reads the release group id as a local album', async () => {
			chooseRemaster();
			await render(AlbumRoute, {
				props: { data: { albumId } }
			} as unknown as Parameters<typeof render<typeof AlbumRoute>>[1]);
			await expect
				.element(page.getByRole('heading', { name: 'Daylight Hours', level: 1 }))
				.toBeVisible();

			expect(mockDetailIdsRead.has(localAlbumId)).toBe(true);
			expect(mockDetailIdsRead.has(albumId)).toBe(false);
		});
	});
});
