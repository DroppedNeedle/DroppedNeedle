import { cursorQuery } from '$lib/features/library/queries/listQueries';
import { pageQuery } from '$lib/features/library/queries/listQueries';
import { tracksQuery } from '$lib/features/library/queries/listQueries';
import { artistsQuery } from '$lib/features/library/queries/listQueries';
import { albumsQuery } from '$lib/features/library/queries/listQueries';
import { v3 } from '$lib/api/v3/endpoint';
import {
	createInfiniteQuery,
	createQuery,
	keepPreviousData,
	queryOptions
} from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import type { V3Body } from '$lib/api/v3/client';
import {
	LibraryQueryKeyFactory,
	type LibraryV3AlbumsParams,
	type LibraryV3ArtistsParams,
	type LibraryV3TracksParams,
	type LibraryV3UserId
} from './LibraryQueryKeyFactory';
import { toMusicBrainzAlbums } from '$lib/queries/search/SearchV3Adapters';
import { fetchLibraryTracks, type LibraryTrackPage } from './libraryTracks';
import type { components } from '$lib/api/v3/openapi';
import type {
	Album,
	AlbumDetailView,
	AlbumView,
	ArtistSort,
	ArtistView,
	LibraryAlbumStatus,
	LibraryArtistScope,
	LibraryMembershipResponse,
	TrackView
} from '$lib/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { PERSIST } from '$lib/queries/persistedCache';

// Catalog reads for the local library. Keys come from the v3 key set, which
// carries the user id because the views include per-caller favorite flags.

const userId = () => authStore.user?.id;

export const getLibraryMembershipQueryOptions = (
	userId: string | undefined,
	identifiers: string[]
) => {
	const albumIds = identifiers
		.map((id) => id.trim().toLowerCase())
		.filter((id, index, allIds) => Boolean(id) && allIds.indexOf(id) === index)
		.sort();
	return queryOptions({
		enabled: Boolean(userId && albumIds.length),
		queryKey: LibraryQueryKeyFactory.membership(userId, albumIds),
		queryFn: async ({ signal }) => {
			let ownedIds: string[] = [];
			let requestedIds: string[] = [];
			let heldTracks: LibraryMembershipResponse['held_tracks'] = [];
			for (let offset = 0; offset < albumIds.length; offset += 500) {
				const membership: LibraryMembershipResponse = await api.v3.POST(
					v3('/api/v3/library/membership'),
					{ album_ids: albumIds.slice(offset, offset + 500) },
					{ signal }
				);
				ownedIds = ownedIds.concat(membership.owned_ids ?? []);
				requestedIds = requestedIds.concat(membership.requested_ids ?? []);
				heldTracks = heldTracks.concat(membership.held_tracks ?? []);
			}
			return {
				owned_ids: ownedIds.sort(),
				requested_ids: requestedIds.sort(),
				held_tracks: heldTracks.sort((a, b) => a.id.localeCompare(b.id))
			};
		}
	});
};

export const getLibraryMembershipQuery = (getAlbumIds: Getter<string[]>) =>
	createQuery(() => getLibraryMembershipQueryOptions(authStore.user?.id, getAlbumIds()));

const getLibraryAlbumsQueryOptions = (forUser: LibraryV3UserId, params: LibraryV3AlbumsParams) =>
	queryOptions({
		placeholderData: keepPreviousData,
		meta: PERSIST,
		queryKey: LibraryQueryKeyFactory.catalog.albums(forUser, params),
		queryFn: ({ signal }) =>
			api.v3.GET(
				v3('/api/v3/library/albums', {
					query: albumsQuery(params, null)
				}),
				{ signal }
			)
	});

export const getLibraryAlbumsQuery = (getParams: Getter<LibraryV3AlbumsParams>) =>
	createQuery(() => getLibraryAlbumsQueryOptions(userId(), getParams()));

type AlbumPage = components['schemas']['AlbumPage'];
type ArtistPage = components['schemas']['ArtistPage'];

/** How many albums each primary format has, for the album list's format filter. */
export const getLibraryAlbumFormatsQuery = () =>
	createQuery(() => ({
		meta: PERSIST,
		queryKey: LibraryQueryKeyFactory.catalog.albumFormats(userId()),
		queryFn: async ({ signal }) =>
			(await api.v3.GET(v3('/api/v3/library/albums/formats'), { signal })).items
	}));

// The album list a page at a time: each page reads on from the last one's
// cursor, so a deep page costs what the first does.
export const getLibraryAlbumsInfiniteQuery = (getParams: Getter<LibraryV3AlbumsParams>) =>
	createInfiniteQuery(() => {
		const params = getParams();
		return {
			placeholderData: keepPreviousData,
			meta: PERSIST,
			queryKey: LibraryQueryKeyFactory.catalog.albumPages(userId(), params),
			initialPageParam: null as string | null,
			queryFn: ({ pageParam, signal }) =>
				api.v3.GET(
					v3('/api/v3/library/albums', {
						query: albumsQuery(params, pageParam)
					}),
					{ signal }
				),
			getNextPageParam: (lastPage: AlbumPage) => lastPage.next ?? undefined
		};
	});

export interface LibraryArtistsParams {
	/** `hearted` is the favourites' own order: the newest heart first. */
	sortBy: ArtistSort | 'hearted';
	sortOrder: 'asc' | 'desc';
	q: string;
	scope: LibraryArtistScope;
	/** Only artists with at least this many tracks in the library. */
	minTracks?: number;
	favorites?: boolean;
}

const ARTISTS_PAGE_SIZE = 48;

function artistParams(params: LibraryArtistsParams): LibraryV3ArtistsParams {
	return {
		limit: ARTISTS_PAGE_SIZE,
		...(params.sortBy === 'hearted' ? {} : { sort: params.sortBy, order: params.sortOrder }),
		q: params.q || undefined,
		scope: params.scope === 'contributors' ? 'contributors' : 'album_artists',
		...(params.minTracks && params.minTracks > 1 ? { minTracks: params.minTracks } : {}),
		...(params.favorites ? { favorites: true } : {})
	};
}

function fetchArtists(
	params: LibraryV3ArtistsParams,
	after: string | null,
	signal?: AbortSignal
): Promise<ArtistPage> {
	return api.v3.GET(
		v3('/api/v3/library/artists', {
			query: artistsQuery(params, after)
		}),
		{ signal }
	);
}

export const getLibraryArtistsInfiniteQuery = (getParams: Getter<LibraryArtistsParams>) =>
	createInfiniteQuery(() => {
		const params = artistParams(getParams());
		return {
			meta: PERSIST,
			queryKey: LibraryQueryKeyFactory.catalog.artists(userId(), params),
			initialPageParam: null as string | null,
			queryFn: ({ pageParam, signal }) => fetchArtists(params, pageParam, signal),
			getNextPageParam: (lastPage: ArtistPage) => lastPage.next ?? undefined
		};
	});

// The track list a page at a time, read on by cursor.
export const getLibraryTracksInfiniteQuery = (getParams: Getter<LibraryV3TracksParams>) =>
	createInfiniteQuery(() => {
		const params = getParams();
		return {
			placeholderData: keepPreviousData,
			meta: PERSIST,
			queryKey: LibraryQueryKeyFactory.catalog.tracks(userId(), params),
			initialPageParam: null as string | null,
			queryFn: ({ pageParam, signal }) => fetchLibraryTracks(params, pageParam, signal),
			getNextPageParam: (lastPage: LibraryTrackPage) => lastPage.next ?? undefined
		};
	});

// separate from the paginated browse query so the hub avoids pulling a full 48-item page for a few thumbnails
const ARTIST_THUMBS_LIMIT = 12;

export const getLibraryArtistThumbsQuery = () =>
	createQuery(() => ({
		meta: PERSIST,
		queryKey: LibraryQueryKeyFactory.catalog.artistThumbs(userId()),
		queryFn: ({ signal }) =>
			fetchArtists({ limit: ARTIST_THUMBS_LIMIT, sort: 'album_count', order: 'desc' }, null, signal)
	}));

const getLibraryStatsQueryOptions = (forUser: LibraryV3UserId) =>
	queryOptions({
		meta: PERSIST,
		queryKey: LibraryQueryKeyFactory.catalog.stats(forUser),
		queryFn: async ({ signal }) => await api.v3.GET(v3('/api/v3/library/stats'), { signal })
	});

export const getLibraryStatsQuery = () => createQuery(() => getLibraryStatsQueryOptions(userId()));

const RECENTLY_ADDED_LIMIT = 20;

export const getLibraryRecentlyAddedQuery = () =>
	createQuery(() => ({
		meta: PERSIST,
		queryKey: LibraryQueryKeyFactory.catalog.recentlyAdded(userId(), RECENTLY_ADDED_LIMIT),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/library/recently-added', { query: { limit: RECENTLY_ADDED_LIMIT } }), {
				signal
			})
	}));

export const getLibraryAlbumDetailQueryOptions = (forUser: LibraryV3UserId, albumId: string) =>
	queryOptions({
		meta: PERSIST,
		queryKey: LibraryQueryKeyFactory.catalog.albumDetail(forUser, albumId),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/library/albums/{id}', { path: { id: albumId } }), { signal })
	});

const HAND_BACK_POLL_MS = 5_000;

export const getLibraryAlbumDetailQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			...getLibraryAlbumDetailQueryOptions(userId(), albumId),
			enabled: !!albumId,
			// Read again until the album's turn to be handed back has come.
			refetchInterval: (query: { state: { data?: AlbumDetailView } }) =>
				query.state.data?.management_hold?.handing_back ? HAND_BACK_POLL_MS : false
		};
	});

// Album pages are reachable by local id and by release-group id. Before a
// local album's page moves to its release group's address, its detail is
// stored (and persisted) under its own local id, where the matched page that
// follows reads it.
export const cacheCanonicalLibraryAlbumDetail = (album: AlbumDetailView) =>
	setQueryDataWithPersister<AlbumDetailView>(
		LibraryQueryKeyFactory.catalog.albumDetail(userId(), album.id),
		album
	);

const getLibraryAlbumCopiesQueryOptions = (forUser: LibraryV3UserId, albumId: string) =>
	queryOptions({
		queryKey: LibraryQueryKeyFactory.catalog.albumCopies(forUser, albumId),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/library/albums/{id}/copies', { path: { id: albumId } }), {
				signal
			})
	});

export const getLibraryAlbumCopiesQuery = (
	getAlbumId: Getter<string>,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			...getLibraryAlbumCopiesQueryOptions(userId(), albumId),
			enabled: getEnabled() && !!albumId
		};
	});

const ALBUM_TRACKS_PAGE = { limit: 500 };

export const getLibraryAlbumTracksQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			enabled: !!albumId,
			meta: PERSIST,
			queryKey: LibraryQueryKeyFactory.catalog.albumTracks(userId(), albumId, ALBUM_TRACKS_PAGE),
			queryFn: ({ signal }) =>
				api.v3.GET(
					v3('/api/v3/library/albums/{id}/tracks', {
						path: { id: albumId },
						query: pageQuery(ALBUM_TRACKS_PAGE)
					}),
					{
						signal
					}
				)
		};
	});

export const getLibraryArtistDetailQueryOptions = (forUser: LibraryV3UserId, artistId: string) =>
	queryOptions({
		meta: PERSIST,
		queryKey: LibraryQueryKeyFactory.catalog.artistDetail(forUser, artistId),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/library/artists/{id}', { path: { id: artistId } }), { signal })
	});

export const getLibraryArtistDetailQuery = (getArtistId: Getter<string>) =>
	createQuery(() => {
		const artistId = getArtistId();
		return { ...getLibraryArtistDetailQueryOptions(userId(), artistId), enabled: !!artistId };
	});

export const cacheCanonicalLibraryArtistDetail = (artist: ArtistView) =>
	setQueryDataWithPersister<ArtistView>(
		LibraryQueryKeyFactory.catalog.artistDetail(userId(), artist.id),
		artist
	);

const ARTIST_ALBUMS_PAGE = { limit: 200 };

export const getLibraryArtistAlbumsQuery = (getArtistId: Getter<string>) =>
	createQuery(() => {
		const artistId = getArtistId();
		return {
			enabled: !!artistId,
			meta: PERSIST,
			queryKey: LibraryQueryKeyFactory.catalog.artistAlbums(userId(), artistId, ARTIST_ALBUMS_PAGE),
			queryFn: ({ signal }) =>
				api.v3.GET(
					v3('/api/v3/library/artists/{id}/albums', {
						path: { id: artistId },
						query: cursorQuery(ARTIST_ALBUMS_PAGE)
					}),
					{
						signal
					}
				)
		};
	});

const ARTIST_APPEARANCES_PAGE_SIZE = 20;

interface ArtistAppearancesPage {
	items: { album: AlbumView; tracks: TrackView[] }[];
	total: number;
	total_tracks: number;
	/** The cursor of the page after this one; null on the last page. */
	next: string | null;
}

// v3 lists the albums an artist appears on; the tracks they appear on come
// from the track list filtered by album and artist, one read per album. The
// artist read first turns a MusicBrainz id into the library artist's id,
// which the track filter needs.
async function fetchAppearances(
	id: string,
	after: string | null,
	signal: AbortSignal
): Promise<ArtistAppearancesPage> {
	const artistId = (
		await api.v3.GET(v3('/api/v3/library/artists/{id}', { path: { id: id } }), { signal })
	).id;
	const page = await api.v3.GET(
		v3('/api/v3/library/artists/{id}/appearances', {
			path: { id: artistId },
			query: cursorQuery({ limit: ARTIST_APPEARANCES_PAGE_SIZE, after })
		}),
		{ signal }
	);
	const items = await Promise.all(
		page.items.map(async (album) => {
			const tracks = await api.v3.GET(
				v3('/api/v3/library/tracks', {
					query: tracksQuery(
						{
							limit: 200,
							sort: 'title',
							order: 'asc',
							albumId: album.id,
							artistId
						},
						null
					)
				}),
				{ signal }
			);
			return { album, tracks: tracks.items };
		})
	);
	return {
		items,
		total: page.total,
		total_tracks: items.reduce((sum, item) => sum + item.tracks.length, 0),
		next: page.next ?? null
	};
}

export const getLibraryArtistAppearancesQuery = (getArtistId: Getter<string>) =>
	createInfiniteQuery(() => {
		const artistId = getArtistId();
		return {
			enabled: !!artistId,
			meta: PERSIST,
			queryKey: LibraryQueryKeyFactory.catalog.artistAppearances(userId(), artistId, {
				limit: ARTIST_APPEARANCES_PAGE_SIZE
			}),
			initialPageParam: null as string | null,
			queryFn: ({ pageParam, signal }) => fetchAppearances(artistId, pageParam, signal),
			getNextPageParam: (lastPage: ArtistAppearancesPage) => lastPage.next ?? undefined
		};
	});

// schedule route is admin-gated; pass `enabled` to keep it off for non-admins
export const getLibraryScanScheduleQuery = (enabled: () => boolean = () => true) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.scanSchedule(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/library/schedule'), { signal })
	}));

// What the library holds for one album, by MusicBrainz id or local id, with
// each held track's quality tier against the upgrade cutoff.
export const getLibraryAlbumStatusQueryOptions = (mbid: string) =>
	queryOptions({
		queryKey: LibraryQueryKeyFactory.album(authStore.user?.id, mbid),
		queryFn: ({ signal }): Promise<LibraryAlbumStatus> =>
			api.v3.GET(v3('/api/v3/library/albums/{id}/status', { path: { id: mbid } }), {
				signal
			})
	});

export const getLibraryAlbumStatusQuery = (getMbid: Getter<string>) =>
	createQuery(() => getLibraryAlbumStatusQueryOptions(getMbid()));

interface LibrarySearchResults {
	albums: AlbumView[];
	artists: ArtistView[];
	tracks: TrackView[];
}

const LIBRARY_SEARCH_LIMIT = 6;

// fans out to album/artist/track endpoints in parallel since there's no combined endpoint; keepPreviousData avoids flashing empty mid-flight
export const getLibrarySearchQuery = (getTerm: Getter<string>) =>
	createQuery(() => {
		const term = getTerm().trim();
		return {
			enabled: term.length >= 2,
			placeholderData: keepPreviousData,
			queryKey: LibraryQueryKeyFactory.catalog.search(userId(), term),
			queryFn: async ({ signal }): Promise<LibrarySearchResults> => {
				const page = { limit: LIBRARY_SEARCH_LIMIT, q: term } as const;
				const [albums, artists, tracks] = await Promise.all([
					api.v3.GET(
						v3('/api/v3/library/albums', {
							query: albumsQuery({ ...page, sort: 'date_added', order: 'desc' }, null)
						}),
						{
							signal
						}
					),
					api.v3.GET(
						v3('/api/v3/library/artists', {
							query: artistsQuery({ ...page, sort: 'name', order: 'asc' }, null)
						}),
						{
							signal
						}
					),
					api.v3.GET(
						v3('/api/v3/library/tracks', {
							query: tracksQuery({ ...page, sort: 'date_added', order: 'desc' }, null)
						}),
						{
							signal
						}
					)
				]);
				return {
					albums: albums.items,
					artists: artists.items,
					tracks: tracks.items
				};
			}
		};
	});

// MusicBrainz album search, used to match dropped files to a release group.
// Only rows with a real release-group MBID are offered.
export const getAlbumSearchQuery = (getTerm: Getter<string>) =>
	createQuery(() => {
		const term = getTerm().trim();
		return {
			enabled: term.length >= 2,
			queryKey: LibraryQueryKeyFactory.catalog.albumSearch(userId(), term),
			queryFn: async ({ signal }): Promise<Album[]> => {
				const data = await api.v3.GET(
					v3('/api/v3/search/{bucket}', {
						path: { bucket: 'albums' },
						query: { q: term.trim(), limit: 20, offset: 0 }
					}),
					{
						signal
					}
				);
				return toMusicBrainzAlbums(data.results);
			}
		};
	});

export type ResolveTracksItem = NonNullable<
	V3Body<'/api/v3/library/resolve-tracks', 'post'>['items']
>[number];

/** The playable copy of each listed track, where the library or a source has one. */
export const getResolvedTracksQuery = (getItems: Getter<ResolveTracksItem[]>) =>
	createQuery(() => {
		const items = getItems();
		return {
			enabled: items.length > 0,
			queryKey: LibraryQueryKeyFactory.catalog.resolvedTracks(authStore.user?.id, items),
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.v3.POST(v3('/api/v3/library/resolve-tracks'), { items }, { signal })
		};
	});
