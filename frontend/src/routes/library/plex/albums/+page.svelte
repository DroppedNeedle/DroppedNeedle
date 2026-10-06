<script lang="ts">
	import { page } from '$app/stores';
	import { api } from '$lib/api/client';
	import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
	import { remoteApi } from '$lib/queries/remotes/remoteApi';
	import { toPlexAlbum, toPlexTrack } from '$lib/queries/remotes/remoteAdapters';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { buildQueueItemsFromPlex } from '$lib/player/queueHelpers';
	import { launchPlexPlayback } from '$lib/player/launchPlexPlayback';
	import {
		getPlexSidebarCachedData,
		getPlexAlbumsListCachedData,
		isPlexSidebarCacheStale,
		isPlexAlbumsListCacheStale,
		setPlexAlbumsListCachedData,
		setPlexSidebarCachedData
	} from '$lib/utils/plexLibraryCache';
	import {
		createLibraryController,
		type LibraryAdapter,
		type SidebarData
	} from '$lib/utils/libraryController.svelte';
	import LibraryPage from '$lib/components/LibraryPage.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import type { PlexAlbumSummary, PlexLibraryStats } from '$lib/types';

	async function fetchAlbumTracks(albumId: string) {
		const page = await remoteApi.albumTracks('plex', albumId, { limit: 500 });
		return page.items.map(toPlexTrack);
	}

	const DECADES = ['2020s', '2010s', '2000s', '1990s', '1980s', '1970s', '1960s'];

	const adapter: LibraryAdapter<PlexAlbumSummary> = {
		sourceType: 'plex',

		getAlbumId: (a) => a.plex_id,
		getAlbumName: (a) => a.name,
		getArtistName: (a) => a.artist_name,
		getAlbumMbid: (a) => a.musicbrainz_id ?? undefined,
		getAlbumImageUrl: (a) => a.image_url ?? null,
		getAlbumYear: (a) => a.year ?? null,

		async fetchAlbums({ limit, offset, sortBy, sortOrder, genre, mood, decade, search, signal }) {
			if (search) {
				const data = await api.v3.GET(REMOTE_ENDPOINTS.search('plex', { q: search }), {
					signal
				});
				const items = data.albums.map(toPlexAlbum);
				return { items, total: items.length };
			}
			const data = await remoteApi.albums(
				'plex',
				{
					limit,
					offset,
					sort_by: sortBy,
					sort_order: sortOrder,
					genre,
					mood,
					decade
				},
				signal
			);
			return { items: data.items.map(toPlexAlbum), total: data.total };
		},

		async fetchSidebarData(signal, current) {
			const [recentRes, genreRes, moodsRes, statsRes] = await Promise.allSettled([
				remoteApi.recent('plex', {}, signal),
				remoteApi.genres('plex', signal),
				api.v3.GET(REMOTE_ENDPOINTS.moods('plex'), { signal }),
				remoteApi.stats('plex', signal)
			]);
			const hasFreshData =
				recentRes.status === 'fulfilled' ||
				genreRes.status === 'fulfilled' ||
				moodsRes.status === 'fulfilled' ||
				statsRes.status === 'fulfilled';
			return {
				data: {
					recentAlbums:
						recentRes.status === 'fulfilled'
							? recentRes.value.map(toPlexAlbum)
							: current.recentAlbums,
					favoriteAlbums: current.favoriteAlbums,
					genres: genreRes.status === 'fulfilled' ? genreRes.value : current.genres,
					moods: moodsRes.status === 'fulfilled' ? moodsRes.value : current.moods,
					stats:
						statsRes.status === 'fulfilled'
							? (statsRes.value as unknown as Record<string, unknown>)
							: current.stats
				},
				hasFreshData
			};
		},

		async fetchAlbumQueueItems(album) {
			const tracks = await fetchAlbumTracks(album.plex_id);
			if (tracks.length === 0) return [];
			const sorted = [...tracks].sort((a, b) => a.track_number - b.track_number);
			return buildQueueItemsFromPlex(sorted, {
				albumId: album.musicbrainz_id || album.plex_id,
				albumName: album.name,
				artistName: album.artist_name,
				coverUrl: album.image_url ?? null,
				artistId: album.artist_musicbrainz_id ?? undefined
			});
		},

		async launchPlayback(album, shuffle) {
			const tracks = await fetchAlbumTracks(album.plex_id);
			if (tracks.length === 0) return;
			launchPlexPlayback(tracks, 0, shuffle, {
				albumId: album.musicbrainz_id || album.plex_id,
				albumName: album.name,
				artistName: album.artist_name,
				coverUrl: getCoverUrl(album.image_url ?? null, album.musicbrainz_id || album.plex_id)
			});
		},

		getAlbumsListCached: (key) => getPlexAlbumsListCachedData(key),
		setAlbumsListCached: (key, data) => setPlexAlbumsListCachedData(data, key),
		isAlbumsListCacheStale: (ts) => isPlexAlbumsListCacheStale(ts),
		getSidebarCached: () => {
			const c = getPlexSidebarCachedData();
			if (!c) return null;
			return {
				data: {
					...c.data,
					favoriteAlbums: [],
					genres: c.data.genres ?? [],
					moods: c.data.moods ?? []
				} as SidebarData<PlexAlbumSummary>,
				timestamp: c.timestamp
			};
		},
		setSidebarCached: (data) =>
			setPlexSidebarCachedData({
				recentAlbums: data.recentAlbums,
				genres: data.genres,
				moods: data.moods ?? [],
				stats: data.stats as PlexLibraryStats | null
			}),
		isSidebarCacheStale: (ts) => isPlexSidebarCacheStale(ts),

		sortOptions: [
			{ value: 'name', label: 'Name' },
			{ value: 'date_added', label: 'Date Added' },
			{ value: 'year', label: 'Year' },
			{ value: 'play_count', label: 'Play Count' },
			{ value: 'rating', label: 'Rating' },
			{ value: 'last_played', label: 'Last Played' }
		],
		defaultSortBy: 'name',
		ascValue: 'asc',
		descValue: 'desc',
		getDefaultSortOrder: (field) => (field === 'name' ? 'asc' : 'desc'),
		supportsGenres: true,
		supportsMoods: true,
		supportsDecades: true,
		supportsTags: false,
		supportsFavorites: false,
		supportsShuffle: true,
		errorMessage: "Couldn't connect to Plex."
	};

	const validSorts = new Set(adapter.sortOptions.map((o) => o.value));
	const urlSort = $page.url.searchParams.get('sort');
	const initialSortBy = urlSort && validSorts.has(urlSort) ? urlSort : undefined;

	const ctrl = createLibraryController(adapter, { initialSortBy });
</script>

<LibraryPage
	{ctrl}
	headerTitle="Plex Library"
	backHref="/library/plex"
	contextMenuBackdrop
	emptyTitle="No albums found"
	emptyDescription="Make sure Plex is set up and has at least one music library."
	decades={DECADES}
>
	{#snippet headerIcon()}
		<span style="color: rgb(var(--brand-plex));">
			<PlexIcon class="h-8 w-8" />
		</span>
	{/snippet}

	{#snippet cardTopLeftBadge(album)}
		<div
			class="badge badge-sm gap-1"
			style="background-color: rgb(var(--brand-plex)); color: #000;"
		>
			<PlexIcon class="h-3 w-3" />
		</div>
		{#if !album.musicbrainz_id}
			<div
				class="badge badge-sm badge-warning gap-1 opacity-80"
				title="Not matched in library - search only"
			>
				?
			</div>
		{/if}
	{/snippet}

	{#snippet emptyIcon()}
		<PlexIcon class="h-12 w-12 opacity-20" />
	{/snippet}
</LibraryPage>
