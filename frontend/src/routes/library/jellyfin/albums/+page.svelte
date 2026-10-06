<script lang="ts">
	import { api } from '$lib/api/client';
	import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
	import { remoteApi } from '$lib/queries/remotes/remoteApi';
	import { toJellyfinAlbum, toJellyfinTrack } from '$lib/queries/remotes/remoteAdapters';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { buildQueueItemsFromJellyfin } from '$lib/player/queueHelpers';
	import { launchJellyfinPlayback } from '$lib/player/launchJellyfinPlayback';
	import {
		getJellyfinSidebarCachedData,
		getJellyfinAlbumsListCachedData,
		isJellyfinSidebarCacheStale,
		isJellyfinAlbumsListCacheStale,
		setJellyfinAlbumsListCachedData,
		setJellyfinSidebarCachedData
	} from '$lib/utils/jellyfinLibraryCache';
	import {
		createLibraryController,
		type LibraryAdapter,
		type SidebarData
	} from '$lib/utils/libraryController.svelte';
	import LibraryPage from '$lib/components/LibraryPage.svelte';
	import type { JellyfinAlbumSummary, JellyfinLibraryStats } from '$lib/types';
	import { Tv } from 'lucide-svelte';

	async function fetchAlbumTracks(albumId: string) {
		const page = await remoteApi.albumTracks('jellyfin', albumId, { limit: 500 });
		return page.items.map(toJellyfinTrack);
	}

	const adapter: LibraryAdapter<JellyfinAlbumSummary> = {
		sourceType: 'jellyfin',

		getAlbumId: (a) => a.jellyfin_id,
		getAlbumName: (a) => a.name,
		getArtistName: (a) => a.artist_name,
		getAlbumMbid: (a) => a.musicbrainz_id ?? undefined,
		getAlbumImageUrl: (a) => a.image_url ?? null,
		getAlbumYear: (a) => a.year,

		async fetchAlbums({ limit, offset, sortBy, sortOrder, genre, search, signal }) {
			if (search) {
				const data = await api.v3.GET(REMOTE_ENDPOINTS.search('jellyfin', { q: search }), {
					signal
				});
				const items = data.albums.map(toJellyfinAlbum);
				return { items, total: items.length };
			}
			const data = await remoteApi.albums(
				'jellyfin',
				{
					limit,
					offset,
					sort_by: sortBy,
					sort_order: sortOrder,
					genre
				},
				signal
			);
			return { items: data.items.map(toJellyfinAlbum), total: data.total };
		},

		async fetchSidebarData(signal, current) {
			const [recentRes, favRes, genreRes, statsRes] = await Promise.allSettled([
				remoteApi.recent('jellyfin', {}, signal),
				api.v3.GET(REMOTE_ENDPOINTS.favorites('jellyfin'), { signal }),
				remoteApi.genres('jellyfin', signal),
				remoteApi.stats('jellyfin', signal)
			]);
			const hasFreshData =
				recentRes.status === 'fulfilled' ||
				favRes.status === 'fulfilled' ||
				genreRes.status === 'fulfilled' ||
				statsRes.status === 'fulfilled';
			return {
				data: {
					recentAlbums:
						recentRes.status === 'fulfilled'
							? recentRes.value.map(toJellyfinAlbum)
							: current.recentAlbums,
					favoriteAlbums:
						favRes.status === 'fulfilled'
							? favRes.value.albums.map(toJellyfinAlbum)
							: current.favoriteAlbums,
					genres: genreRes.status === 'fulfilled' ? genreRes.value : current.genres,
					moods: [],
					stats:
						statsRes.status === 'fulfilled'
							? (statsRes.value as unknown as Record<string, unknown>)
							: current.stats
				},
				hasFreshData
			};
		},

		async fetchAlbumQueueItems(album) {
			const tracks = await fetchAlbumTracks(album.jellyfin_id);
			if (tracks.length === 0) return [];
			const sorted = [...tracks].sort((a, b) => a.track_number - b.track_number);
			return buildQueueItemsFromJellyfin(sorted, {
				albumId: album.musicbrainz_id || album.jellyfin_id,
				albumName: album.name,
				artistName: album.artist_name,
				coverUrl: album.image_url ?? null,
				artistId: album.artist_musicbrainz_id ?? undefined
			});
		},

		async launchPlayback(album, shuffle) {
			const tracks = await fetchAlbumTracks(album.jellyfin_id);
			if (tracks.length === 0) return;
			launchJellyfinPlayback(tracks, 0, shuffle, {
				albumId: album.musicbrainz_id || album.jellyfin_id,
				albumName: album.name,
				artistName: album.artist_name,
				coverUrl: getCoverUrl(album.image_url, album.musicbrainz_id || album.jellyfin_id)
			});
		},

		getAlbumsListCached: (key) => getJellyfinAlbumsListCachedData(key),
		setAlbumsListCached: (key, data) => setJellyfinAlbumsListCachedData(data, key),
		isAlbumsListCacheStale: (ts) => isJellyfinAlbumsListCacheStale(ts),
		getSidebarCached: () => {
			const c = getJellyfinSidebarCachedData();
			if (!c) return null;
			return {
				data: {
					...c.data,
					favoriteAlbums: c.data.favoriteAlbums ?? [],
					genres: c.data.genres ?? [],
					moods: []
				} as SidebarData<JellyfinAlbumSummary>,
				timestamp: c.timestamp
			};
		},
		setSidebarCached: (data) =>
			setJellyfinSidebarCachedData({
				recentAlbums: data.recentAlbums,
				favoriteAlbums: data.favoriteAlbums,
				genres: data.genres,
				stats: data.stats as JellyfinLibraryStats | null
			}),
		isSidebarCacheStale: (ts) => isJellyfinSidebarCacheStale(ts),

		sortOptions: [
			{ value: 'SortName', label: 'Name' },
			{ value: 'DateCreated', label: 'Date Added' },
			{ value: 'ProductionYear', label: 'Year' }
		],
		defaultSortBy: 'SortName',
		ascValue: 'asc',
		descValue: 'desc',
		getDefaultSortOrder: (field) => (field === 'SortName' ? 'asc' : 'desc'),
		supportsGenres: true,
		supportsMoods: false,
		supportsDecades: false,
		supportsTags: false,
		supportsFavorites: true,
		supportsShuffle: true,
		errorMessage: "Couldn't connect to Jellyfin."
	};

	const ctrl = createLibraryController(adapter);
</script>

<LibraryPage
	{ctrl}
	headerTitle="Jellyfin Library"
	backHref="/library/jellyfin"
	emptyTitle="No albums found"
	emptyDescription="Make sure Jellyfin is set up and has at least one music library."
>
	{#snippet headerIcon()}
		<Tv class="h-8 w-8 text-info" />
	{/snippet}

	{#snippet cardTopLeftBadge(_album)}
		<div class="badge badge-sm gap-1 badge-info">
			<Tv class="h-3 w-3" />
		</div>
	{/snippet}

	{#snippet emptyIcon()}
		<Tv class="h-12 w-12 opacity-20" />
	{/snippet}
</LibraryPage>
