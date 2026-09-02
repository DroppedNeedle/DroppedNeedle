import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	fetchNextPage: vi.fn(),
	connectionsData: { connections: [] } as
		| {
				connections: Array<{ service: string; enabled: boolean; username: string }>;
		  }
		| undefined,
	connectionsPending: false,
	connectionsError: null as unknown,
	persistedSource: undefined as string | undefined,
	persistedState: null as { current: unknown } | null,
	discoveryCalls: {
		similarArtists: 0,
		topAlbums: 0,
		topSongs: 0
	},
	discoveryParams: {
		similarArtists: null as { source: string; enabled?: boolean } | null,
		topAlbums: null as { source: string; enabled?: boolean } | null,
		topSongs: null as { source: string; enabled?: boolean } | null
	},
	extended: {
		data: {} as { description?: string } | undefined,
		isLoading: false,
		isRefetching: false,
		error: null
	}
}));

type DiscoveryKind = 'similarArtists' | 'topAlbums' | 'topSongs';
type DiscoveryParams = { source: string; enabled?: boolean };

function captureDiscoveryQuery(kind: DiscoveryKind, getParams: () => DiscoveryParams) {
	const rawParams = getParams();
	const params: DiscoveryParams = { source: rawParams.source };
	if (rawParams.enabled !== undefined) params.enabled = rawParams.enabled;
	h.discoveryParams[kind] = params;
	if (params.enabled === true) h.discoveryCalls[kind] += 1;
	return {
		data: { similar_artists: [], albums: [], songs: [] },
		isLoading: params.enabled !== true
	};
}

function emptyComponent() {
	const Component = function () {};
	Component.prototype = {};
	return { default: Component };
}

vi.mock('$lib/components/ArtistHeaderSkeleton.svelte', emptyComponent);
vi.mock('$lib/components/AlbumGridSkeleton.svelte', emptyComponent);
vi.mock('$lib/components/ArtistWhereToBuy.svelte', emptyComponent);
vi.mock('$lib/components/ReleaseList.svelte', emptyComponent);
vi.mock('$lib/components/Toast.svelte', emptyComponent);
vi.mock('$lib/components/ArtistHero.svelte', emptyComponent);
vi.mock('$lib/components/SimilarArtistsCarousel.svelte', emptyComponent);
vi.mock('$lib/components/TopSongsList.svelte', emptyComponent);
vi.mock('$lib/components/TopAlbumsList.svelte', emptyComponent);
vi.mock('$lib/queries/connections/ConnectionsQuery.svelte', () => ({
	getConnectionsQuery: () => ({
		get data() {
			return h.connectionsData;
		},
		get isPending() {
			return h.connectionsPending;
		},
		get isError() {
			return h.connectionsError !== null;
		},
		get isSuccess() {
			return (
				h.connectionsData !== undefined && !h.connectionsPending && h.connectionsError === null
			);
		}
	})
}));

vi.mock('runed', () => ({
	PersistedState: class {
		current: unknown;

		constructor(_key: string, initial: unknown) {
			this.current = h.persistedSource ?? initial;
			h.persistedState = this;
		}
	}
}));

vi.mock('$lib/components/LastFmEnrichment.svelte', emptyComponent);
vi.mock('$lib/components/LibraryAlbumsCarousel.svelte', emptyComponent);
vi.mock('$lib/components/library/ArtistAppearancesSection.svelte', emptyComponent);
vi.mock('$lib/components/PageSectionToc.svelte', emptyComponent);
vi.mock('$lib/components/SimpleSourceSwitcher.svelte', emptyComponent);
vi.mock('$lib/components/ArtistPlaybackBar.svelte', emptyComponent);

vi.mock('$lib/queries/artist/ArtistQueries.svelte', () => ({
	getBasicArtistQuery: () => ({
		data: {
			name: 'Pagination Fixture',
			musicbrainz_id: 'artist-1',
			tags: [],
			aliases: [],
			external_links: [],
			in_library: false,
			appears_in_library: false
		},
		isLoading: false,
		isRefetching: false,
		error: null
	}),
	getExtendedArtistQuery: () => h.extended,
	getSimilarArtistsQuery: (getParams: () => DiscoveryParams) =>
		captureDiscoveryQuery('similarArtists', getParams),
	getArtistTopAlbumsQuery: (getParams: () => DiscoveryParams) =>
		captureDiscoveryQuery('topAlbums', getParams),
	getArtistTopSongsQuery: (getParams: () => DiscoveryParams) =>
		captureDiscoveryQuery('topSongs', getParams),
	getArtistLastFmEnrichmentQuery: () => ({ data: undefined, isLoading: false }),
	getArtistReleasesInfiniteQuery: () => ({
		data: {
			pages: [
				{
					albums: [
						{
							id: 'release-1',
							title: 'First page release',
							year: 2026,
							in_library: false
						}
					],
					eps: [],
					singles: [],
					offset: 0,
					limit: 50,
					returned_count: 1,
					next_offset: 100,
					has_more: true,
					source_total_count: 240
				}
			]
		},
		isLoading: false,
		isFetchingNextPage: false,
		hasNextPage: true,
		fetchNextPage: (...args: unknown[]) => h.fetchNextPage(...args)
	}),
	updateArtistReleaseInCache: vi.fn()
}));
vi.mock('$lib/queries/downloads/DownloadMutations.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/downloads/DownloadMutations.svelte')>()),
	requestAlbum: () => ({
		mutateAsync: vi.fn().mockResolvedValue({ success: true }),
		isPending: false
	})
}));
vi.mock('$lib/stores/musicSource', () => ({
	isMusicSource: (value: unknown) => value === 'listenbrainz' || value === 'lastfm'
}));
import ProviderArtistPage from './ProviderArtistPage.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.connectionsData = { connections: [] };
	h.connectionsPending = false;
	h.connectionsError = null;
	h.persistedSource = undefined;
	h.persistedState = null;
	h.discoveryCalls = {
		similarArtists: 0,
		topAlbums: 0,
		topSongs: 0
	};
	h.discoveryParams = {
		similarArtists: null,
		topAlbums: null,
		topSongs: null
	};
	h.extended = {
		data: {},
		isLoading: false,
		isRefetching: false,
		error: null
	};
});
afterEach(() => vi.useRealTimers());

it('does not fetch a second release page until the user asks for it', async () => {
	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	const button = page.getByRole('button', { name: 'Load more releases' });
	await expect.element(button).toBeVisible();
	vi.useFakeTimers();
	await vi.advanceTimersByTimeAsync(30_000);
	expect(h.fetchNextPage).not.toHaveBeenCalled();
	vi.useRealTimers();

	await button.click();
	expect(h.fetchNextPage).toHaveBeenCalledTimes(1);
});

it('renders available biography data even while the query still reports loading', async () => {
	h.extended = {
		data: { description: 'The finished biography.' },
		isLoading: true,
		isRefetching: false,
		error: null
	};

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	await expect.element(page.getByText('The finished biography.')).toBeVisible();
	await expect.element(page.getByTestId('artist-description-skeleton')).not.toBeInTheDocument();
});

it('keeps the biography skeleton while extended data is initially unavailable', async () => {
	h.extended = {
		data: undefined,
		isLoading: true,
		isRefetching: false,
		error: null
	};

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	await expect.element(page.getByTestId('artist-description-skeleton')).toBeInTheDocument();
});
it('repairs a stale Last.fm selection to ListenBrainz when only ListenBrainz is linked', async () => {
	h.persistedSource = 'lastfm';
	h.connectionsData = {
		connections: [{ service: 'listenbrainz', enabled: true, username: 'lb-user' }]
	};

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'lastfm' }
	});

	await vi.waitFor(() => expect(h.persistedState?.current).toBe('listenbrainz'));
});

it('repairs a stale ListenBrainz selection to Last.fm when only Last.fm is linked', async () => {
	h.persistedSource = 'listenbrainz';
	h.connectionsData = {
		connections: [{ service: 'lastfm', enabled: true, username: 'lfm-user' }]
	};

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	await vi.waitFor(() => expect(h.persistedState?.current).toBe('lastfm'));
});

it('preserves the intentional source selection when both services are linked', async () => {
	h.persistedSource = 'lastfm';
	h.connectionsData = {
		connections: [
			{ service: 'listenbrainz', enabled: true, username: 'lb-user' },
			{ service: 'lastfm', enabled: true, username: 'lfm-user' }
		]
	};

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	await vi.waitFor(() => expect(h.persistedState?.current).toBe('lastfm'));
	expect(h.discoveryCalls).toEqual({
		similarArtists: 1,
		topAlbums: 1,
		topSongs: 1
	});
	expect(h.discoveryParams.similarArtists?.source).toBe('lastfm');
	expect(h.discoveryParams.topAlbums?.source).toBe('lastfm');
	expect(h.discoveryParams.topSongs?.source).toBe('lastfm');
});

it('does not rewrite a stale selection while connections are pending', async () => {
	h.persistedSource = 'lastfm';
	h.connectionsData = undefined;
	h.connectionsPending = true;

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	expect(h.persistedState?.current).toBe('lastfm');
});

it('does not rewrite a stale selection when no services are linked', async () => {
	h.persistedSource = 'lastfm';
	h.connectionsData = { connections: [] };

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	await vi.waitFor(() => expect(h.persistedState?.current).toBe('lastfm'));
	expect(h.discoveryCalls).toEqual({
		similarArtists: 1,
		topAlbums: 1,
		topSongs: 1
	});
	expect(h.discoveryParams).toEqual({
		similarArtists: { source: 'lastfm', enabled: true },
		topAlbums: { source: 'lastfm', enabled: true },
		topSongs: { source: 'lastfm', enabled: true }
	});
});
it('keeps discovery disabled during a cold connections load without flashing stale source data', async () => {
	h.persistedSource = 'lastfm';
	h.connectionsData = undefined;
	h.connectionsPending = true;

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	expect(h.persistedState?.current).toBe('lastfm');
	expect(h.discoveryCalls).toEqual({
		similarArtists: 0,
		topAlbums: 0,
		topSongs: 0
	});
	expect(h.discoveryParams).toEqual({
		similarArtists: { source: 'lastfm', enabled: false },
		topAlbums: { source: 'lastfm', enabled: false },
		topSongs: { source: 'lastfm', enabled: false }
	});
});

it('enables each discovery section exactly once on the resolved ListenBrainz source', async () => {
	h.persistedSource = 'lastfm';
	h.connectionsData = {
		connections: [{ service: 'listenbrainz', enabled: true, username: 'lb-user' }]
	};

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'lastfm' }
	});

	await vi.waitFor(() => expect(h.persistedState?.current).toBe('listenbrainz'));
	expect(h.discoveryCalls).toEqual({
		similarArtists: 1,
		topAlbums: 1,
		topSongs: 1
	});
	expect(h.discoveryParams).toEqual({
		similarArtists: { source: 'listenbrainz', enabled: true },
		topAlbums: { source: 'listenbrainz', enabled: true },
		topSongs: { source: 'listenbrainz', enabled: true }
	});
});
it('uses the selected source as a graceful fallback when connections fail', async () => {
	h.persistedSource = 'lastfm';
	h.connectionsData = undefined;
	h.connectionsError = new Error('connections unavailable');

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	await vi.waitFor(() => expect(h.persistedState?.current).toBe('lastfm'));
	expect(h.discoveryCalls).toEqual({
		similarArtists: 1,
		topAlbums: 1,
		topSongs: 1
	});
	expect(h.discoveryParams).toEqual({
		similarArtists: { source: 'lastfm', enabled: true },
		topAlbums: { source: 'lastfm', enabled: true },
		topSongs: { source: 'lastfm', enabled: true }
	});
});

it('uses the selected source when connections settle without response data', async () => {
	h.persistedSource = 'lastfm';
	h.connectionsData = undefined;

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	await vi.waitFor(() => expect(h.persistedState?.current).toBe('lastfm'));
	expect(h.discoveryCalls).toEqual({
		similarArtists: 1,
		topAlbums: 1,
		topSongs: 1
	});
});
