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
	mockAlbumSourceMatchCache: {
		get: vi.fn(),
		set: vi.fn(),
		remove: vi.fn(),
		isStale: vi.fn(() => false)
	},
	mockDownloadsData: { value: undefined as unknown },
	mockHeldData: { value: { items: [] } as unknown },
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
	albumSourceMatchCacheKey: (id: string) => `key:${id}`
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
	}),
	getLibraryAlbumDetailQuery: () => ({
		data: undefined,
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
	getLibraryAlbumsQuery: () => ({ data: { items: [] }, isLoading: false })
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
	})
}));

vi.mock('$lib/queries/albums/EditionQueries.svelte', () => ({
	getAlbumEditionsQuery: () => ({
		get data() {
			return undefined;
		}
	}),
	setEditionPin: () => ({ mutateAsync: vi.fn(), isPending: false }),
	clearEditionPin: () => ({ mutateAsync: vi.fn(), isPending: false }),
	acquireEdition: () => ({ mutateAsync: vi.fn(), isPending: false }),
	getLocalAlbumEditionPinQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	setLocalAlbumEditionPin: () => ({ mutateAsync: vi.fn(), isPending: false }),
	clearLocalAlbumEditionPin: () => ({ mutateAsync: vi.fn(), isPending: false })
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
	removeLibraryTrack: () => mockRemoveTrack
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

vi.mock('$lib/player/launchJellyfinPlayback', () => ({ launchJellyfinPlayback: vi.fn() }));
vi.mock('$lib/player/launchLocalPlayback', () => ({ launchLocalPlayback: vi.fn() }));
vi.mock('$lib/player/launchNavidromePlayback', () => ({ launchNavidromePlayback: vi.fn() }));

import AlbumPage from './ProviderAlbumPage.svelte';
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

	beforeEach(() => {
		mockRemoveTrack.mutate.mockClear();
		mockRemoveTrack.isPending = false;
		authStore.setUser(
			toAuthUser({
				id: 'user-1',
				display_name: 'AdminTest',
				role: 'admin',
				email: null,
				avatar_url: null,
				username: 'AdminTest',
				username_display: 'AdminTest'
			})
		);
	});

	function localMatchFetchCount(): number {
		return mockPageFetch.mock.calls.filter(([url]) =>
			String(url).includes('/api/v1/local/albums/match/')
		).length;
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
		expect(
			mockPageFetch.mock.calls
				.filter(([url]) => String(url).includes('/api/v1/local/albums/match/'))
				.every(([url]) => String(url).endsWith(albumId))
		).toBe(true);
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
		const before = mockPageFetch.mock.calls.filter(([url]) => String(url).endsWith(albumId)).length;
		await options.onSuccess();
		expect(mockPageFetch.mock.calls.filter(([url]) => String(url).endsWith(albumId)).length).toBe(
			before
		);
	});
});
