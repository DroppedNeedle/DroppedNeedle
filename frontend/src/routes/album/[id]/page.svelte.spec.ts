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
	mockLibraryStatusData,
	mockLocalCopiesData
} = vi.hoisted(() => ({
	mockGoto: vi.fn(),
	mockPageFetch: vi.fn(),
	mockHydrateDetailCacheEntry: vi.fn(),
	mockAlbumBasicCache: { set: vi.fn() },
	mockAlbumTracksCache: { set: vi.fn() },
	mockAlbumDiscoveryCache: { set: vi.fn() },
	mockAlbumLastFmCache: { set: vi.fn() },
	mockAlbumYouTubeCache: { get: vi.fn(), set: vi.fn() },
	mockAlbumSourceMatchCache: { get: vi.fn(), set: vi.fn() },
	mockDownloadsData: { value: undefined as unknown },
	mockLibraryStatusData: { value: undefined as unknown },
	mockLocalCopiesData: { value: { items: [] } as unknown }
}));

vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$app/navigation', () => ({
	goto: (...args: unknown[]) => mockGoto(...args)
}));

vi.mock('$lib/stores/library', () => ({
	libraryStore: {
		isInLibrary: vi.fn(() => false),
		isRequested: vi.fn(() => false)
	}
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
	albumSourceMatchCache: mockAlbumSourceMatchCache
}));

vi.mock('$lib/utils/serviceStatus', () => ({
	extractServiceStatus: vi.fn()
}));

// Stub the library status query so the page renders without a QueryClientProvider.
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryAlbumStatusQuery: () => ({
		get data() {
			return mockLibraryStatusData.value;
		},
		refetch: vi.fn()
	}),
	getLibraryAlbumCopiesQuery: () => ({
		get data() {
			return mockLocalCopiesData.value;
		},
		isLoading: false
	})
}));

// Where-to-buy section (Get it): stub so the page renders without a QueryClientProvider
vi.mock('$lib/queries/albums/GetItQueries.svelte', () => ({
	getPurchaseOptionsQuery: () => ({ data: undefined, isLoading: false })
}));

// album-scoped downloads query: feed it per-test via mockDownloadsData so the page renders without
// a QueryClientProvider (same approach as the library status query above)
vi.mock('$lib/queries/downloads/DownloadQueries.svelte', () => ({
	getAlbumDownloadsQuery: () => ({
		get data() {
			return mockDownloadsData.value;
		},
		refetch: vi.fn()
	})
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	cancelDownload: () => ({ mutate: vi.fn(), isPending: false }),
	retryDownload: () => ({ mutate: vi.fn(), isPending: false }),
	stopAutoRetry: () => ({ mutate: vi.fn(), isPending: false }),
	requestTrack: () => ({ mutate: vi.fn(), isPending: false }),
	importHeldTrack: () => ({ mutate: vi.fn(), isPending: false }),
	discardHeldTrack: () => ({ mutate: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/albums/EditionQueries.svelte', () => ({
	getAlbumEditionsQuery: () => ({
		get data() {
			return undefined;
		}
	}),
	setEditionPin: () => ({ mutateAsync: vi.fn(), isPending: false }),
	clearEditionPin: () => ({ mutateAsync: vi.fn(), isPending: false }),
	acquireEdition: () => ({ mutateAsync: vi.fn(), isPending: false })
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
			return { items: [] };
		}
	})
}));

vi.mock('$lib/queries/downloads/DownloadSSE.svelte', () => ({
	createDownloadStream: () => ({
		state: { progress: null, status: null, done: false },
		start: vi.fn(),
		stop: vi.fn()
	})
}));

// Stub only the rescan mutation factory so AlbumHeader renders without a QueryClientProvider; keep other exports intact.
vi.mock('$lib/queries/library/LibraryMutations.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/library/LibraryMutations.svelte')>()),
	rescanAlbum: () => ({ mutateAsync: vi.fn(), isPending: false }),
	// the orphan-review section (P5) creates its removal mutation at init - stub it
	// so the page renders without a QueryClientProvider
	removeLibraryTrack: () => ({ mutate: vi.fn(), isPending: false })
}));

vi.mock('$lib/utils/albumRequest', () => ({
	requestAlbum: vi.fn().mockResolvedValue({ success: true })
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
vi.mock('$lib/components/ContextMenu.svelte', () => {
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

vi.mock('$lib/player/launchJellyfinPlayback', () => ({ launchJellyfinPlayback: vi.fn() }));
vi.mock('$lib/player/launchLocalPlayback', () => ({ launchLocalPlayback: vi.fn() }));
vi.mock('$lib/player/launchNavidromePlayback', () => ({ launchNavidromePlayback: vi.fn() }));

import AlbumPage from './ProviderAlbumPage.svelte';
import type { DownloadTask } from '$lib/types';

const albumId = '3f3a6d95-326e-4384-80b0-0744f20f24ff';

function makeTask(overrides: Partial<DownloadTask> = {}): DownloadTask {
	return {
		id: 'task-1',
		user_id: 'user-1',
		download_type: 'album',
		release_group_mbid: albumId,
		recording_mbid: null,
		artist_name: 'Grimes',
		album_title: 'Visions',
		track_title: null,
		year: 2012,
		status: 'downloading',
		progress_percent: 42,
		total_size_bytes: 1000,
		downloaded_bytes: 420,
		files_total: 4,
		files_completed: 1,
		files_failed: 0,
		source_username: 'peer',
		search_job_id: 'job-1',
		candidate_index: 0,
		preflight_score: 0.9,
		final_path: null,
		error_message: null,
		retry_count: 0,
		created_at: 1,
		updated_at: 2,
		completed_at: null,
		next_retry_at: null,
		retry_max: 6,
		retry_ladder_minutes: [15, 30, 60, 120, 240, 480],
		acquisition_cleanup_state: 'not_tracked',
		...overrides
	};
}

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
		mockLibraryStatusData.value = undefined;
		mockLocalCopiesData.value = { items: [] };
		mockHydrateDetailCacheEntry.mockImplementation(({ cache, onHydrate }: HydrateOptions) => {
			if (cache === mockAlbumBasicCache) {
				onHydrate({
					title: 'Visions',
					musicbrainz_id: albumId,
					artist_name: 'Grimes',
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
					country: null
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
		mockPageFetch.mockImplementation((input: string | URL) => {
			const url = typeof input === 'string' ? input : input.toString();

			if (url.endsWith(`/api/v1/albums/${albumId}/basic`)) {
				return Promise.resolve(
					jsonResponse({
						title: 'Visions',
						musicbrainz_id: albumId,
						artist_name: 'Grimes',
						artist_id: 'artist-1',
						in_library: true,
						requested: false,
						cover_url: null
					})
				);
			}

			if (url.endsWith(`/api/v1/albums/${albumId}/tracks`)) {
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
						country: null
					})
				);
			}

			if (url.includes(`/api/v1/albums/${albumId}/more-by-artist`)) {
				return Promise.resolve(jsonResponse({ artist_name: 'Grimes', albums: [] }));
			}

			if (url.includes(`/api/v1/albums/${albumId}/similar`)) {
				return Promise.resolve(jsonResponse({ albums: [] }));
			}

			if (url.endsWith(`/api/v1/youtube/link/${albumId}`)) {
				return Promise.resolve(jsonResponse({ detail: 'not found' }, 404));
			}

			if (url.endsWith(`/api/v1/youtube/track-links/${albumId}`)) {
				return Promise.resolve(jsonResponse([]));
			}

			if (url.endsWith(`/api/v1/jellyfin/albums/match/${albumId}`)) {
				return Promise.resolve(jsonResponse({ found: false, jellyfin_album_id: null, tracks: [] }));
			}

			if (url.endsWith(`/api/v1/local/albums/match/${albumId}`)) {
				return Promise.resolve(
					jsonResponse({
						found: true,
						tracks: [
							{
								track_file_id: 1,
								title: 'Infinite ❤️ Without Fulfillment',
								track_number: 1,
								disc_number: 1,
								duration_seconds: 95,
								size_bytes: 1,
								format: 'flac'
							},
							{
								track_file_id: 2,
								title: 'Circumambient',
								track_number: 5,
								disc_number: 1,
								duration_seconds: 223,
								size_bytes: 1,
								format: 'flac'
							},
							{
								track_file_id: 3,
								title: 'Ambrosia',
								track_number: 1,
								disc_number: 2,
								duration_seconds: 213,
								size_bytes: 1,
								format: 'flac'
							},
							{
								track_file_id: 4,
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
					})
				);
			}

			if (url.includes(`/api/v1/navidrome/album-match/${albumId}`)) {
				return Promise.resolve(
					jsonResponse({
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
								album_name: 'Visions',
								artist_name: 'Grimes'
							},
							{
								navidrome_id: 'n2',
								title: 'Circumambient',
								track_number: 5,
								disc_number: 1,
								duration_seconds: 223,
								codec: 'flac',
								bitrate: 800,
								album_name: 'Visions',
								artist_name: 'Grimes'
							},
							{
								navidrome_id: 'n3',
								title: 'Ambrosia',
								track_number: 1,
								disc_number: 2,
								duration_seconds: 213,
								codec: 'flac',
								bitrate: 800,
								album_name: 'Visions',
								artist_name: 'Grimes'
							},
							{
								navidrome_id: 'n4',
								title: 'Be a Body (Baarsden rework)',
								track_number: 5,
								disc_number: 2,
								duration_seconds: 204,
								codec: 'flac',
								bitrate: 800,
								album_name: 'Visions',
								artist_name: 'Grimes'
							}
						]
					})
				);
			}

			return Promise.resolve(jsonResponse({}));
		});
	});

	it('lets authoritative library status override stale owned album metadata', async () => {
		mockLibraryStatusData.value = {
			in_library: false,
			track_count: 0,
			tracks: [],
			expected_tracks: 4,
			covered_tracks: 0,
			matched_file_ids: [],
			orphans: []
		};

		render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);

		await expect.element(page.getByRole('button', { name: 'Add to Library' })).toBeVisible();
		await expect.element(page.getByText('In Library', { exact: true })).not.toBeInTheDocument();
	});

	it('keeps every local copy visible on the shared provider route', async () => {
		const copy = (id: string, title: string) => ({
			id,
			title,
			artist_name: 'Grimes',
			artist_id: 'local-artist-1',
			musicbrainz_release_group_id: albumId,
			musicbrainz_release_id: 'release-1',
			musicbrainz_artist_id: 'artist-1',
			album_identity_state: 'release_linked',
			track_count: 4,
			total_duration_seconds: 736,
			total_size_bytes: 4,
			format: 'flac',
			year: 2012,
			is_compilation: false,
			cover_available: false,
			date_added: 1,
			sort_name: null,
			original_release_date: null
		});
		mockLocalCopiesData.value = {
			items: [
				copy('local-copy-1', 'Visions, original files'),
				copy('local-copy-2', 'Visions, remaster')
			]
		};

		render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);

		await expect
			.element(page.getByRole('heading', { name: 'Copies in your library' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Open Visions, original files' }))
			.toHaveAttribute('href', `/album/${albumId}`);
		await expect
			.element(page.getByRole('link', { name: 'Open Visions, remaster' }))
			.toHaveAttribute('href', `/album/${albumId}`);
	});

	it('replaces a release alias URL with the canonical release-group URL', async () => {
		const canonicalId = '886755e5-6c76-4181-9a16-0b167ee7bfc3';
		const originalHydrator = mockHydrateDetailCacheEntry.getMockImplementation();
		mockHydrateDetailCacheEntry.mockImplementation(
			(options: { cache: unknown; onHydrate: (value: unknown) => void }) => {
				if (options.cache === mockAlbumBasicCache) {
					options.onHydrate({
						title: 'Pixie Queen',
						musicbrainz_id: canonicalId,
						artist_name: 'Anthony Green',
						artist_id: 'artist-1',
						in_library: true,
						requested: false,
						cover_url: null
					});
					return false;
				}
				return originalHydrator?.(options);
			}
		);

		render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);

		await vi.waitFor(() => {
			expect(mockGoto).toHaveBeenCalledWith(`/album/${canonicalId}`, {
				replaceState: true
			});
		});
	});

	it('renders visible grouped track rows alongside source bars', async () => {
		expect.assertions(6);
		render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);

		await expect.element(page.getByText('Local Files')).toBeVisible();
		await expect.element(page.getByText('Navidrome')).toBeVisible();
		await expect.element(page.getByText('Disc 1')).toBeVisible();
		await expect.element(page.getByText('Disc 2')).toBeVisible();
		await expect.element(page.getByText('Infinite ❤️ Without Fulfillment')).toBeVisible();
		await expect.element(page.getByText('Be a Body (Baarsden rework)')).toBeVisible();
	});

	it('does not refetch tracks when the tracks cache is fresh but basic metadata is stale', async () => {
		expect.assertions(2);
		mockHydrateDetailCacheEntry.mockImplementation(({ cache, onHydrate }: HydrateOptions) => {
			if (cache === mockAlbumBasicCache) {
				onHydrate({
					title: 'Visions',
					musicbrainz_id: albumId,
					artist_name: 'Grimes',
					artist_id: 'artist-1',
					in_library: true,
					requested: false,
					cover_url: null
				});
				return true;
			}

			if (cache === mockAlbumTracksCache) {
				onHydrate({
					tracks: [
						{ position: 1, disc_number: 1, title: 'Infinite ❤️ Without Fulfillment', length: 95327 }
					],
					total_tracks: 1,
					total_length: 95327,
					label: null,
					barcode: null,
					country: null
				});
				return false;
			}

			return true;
		});

		render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);

		await expect.element(page.getByText('Infinite ❤️ Without Fulfillment')).toBeVisible();
		expect(
			mockPageFetch.mock.calls.some(
				([input]) => typeof input === 'string' && input.endsWith(`/api/v1/albums/${albumId}/tracks`)
			)
		).toBe(false);
	});

	it('shows the Pressing download strip while a download is in flight', async () => {
		expect.assertions(2);
		mockDownloadsData.value = {
			items: [makeTask({ status: 'downloading' })],
			page: 1,
			page_size: 100
		};

		render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);

		await expect.element(page.getByText('Downloading')).toBeVisible();
		// the old broken poller badge is gone for good
		expect(page.getByText('Checking for sources…').elements()).toHaveLength(0);
	});

	it('shows no download strip for a settled album with no active download', async () => {
		expect.assertions(2);
		mockDownloadsData.value = { items: [], page: 1, page_size: 100 };

		render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);

		await expect.element(page.getByText('Infinite ❤️ Without Fulfillment')).toBeVisible();
		expect(page.getByText('Downloading').elements()).toHaveLength(0);
	});

	it('shows the per-track pressing vinyl for a downloading track', async () => {
		expect.assertions(1);
		// tracks need a recording_id for the per-track task to match
		mockHydrateDetailCacheEntry.mockImplementation(({ cache, onHydrate }: HydrateOptions) => {
			if (cache === mockAlbumBasicCache) {
				onHydrate({
					title: 'Visions',
					musicbrainz_id: albumId,
					artist_name: 'Grimes',
					artist_id: 'artist-1',
					in_library: false,
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
							length: 95327,
							recording_id: 'rec-1'
						}
					],
					total_tracks: 1,
					total_length: 95327,
					label: null,
					barcode: null,
					country: null
				});
				return false;
			}
			return true;
		});
		mockDownloadsData.value = {
			items: [
				makeTask({
					id: 'task-track-1',
					download_type: 'track',
					recording_mbid: 'rec-1',
					track_title: 'Infinite ❤️ Without Fulfillment',
					status: 'downloading'
				})
			],
			page: 1,
			page_size: 100
		};

		render(AlbumPage, {
			props: { data: { albumId } }
		} as Parameters<typeof render<typeof AlbumPage>>[1]);

		await expect.element(page.getByTitle('Downloading 42%')).toBeVisible();
	});
});
