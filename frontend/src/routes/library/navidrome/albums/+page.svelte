<script lang="ts">
	import { api } from '$lib/api/client';
	import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
	import { remoteApi } from '$lib/queries/remotes/remoteApi';
	import { toNavidromeAlbum, toNavidromeTrack } from '$lib/queries/remotes/remoteAdapters';
	import { getRemoteFoldersQuery } from '$lib/queries/remotes/RemoteQueries.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { buildQueueItemsFromNavidrome } from '$lib/player/queueHelpers';
	import { launchNavidromePlayback } from '$lib/player/launchNavidromePlayback';
	import {
		setNavidromeFolderScopeRevision,
		getNavidromeSidebarCachedData,
		getNavidromeAlbumsListCachedData,
		isNavidromeSidebarCacheStale,
		isNavidromeAlbumsListCacheStale,
		setNavidromeAlbumsListCachedData,
		setNavidromeSidebarCachedData
	} from '$lib/utils/navidromeLibraryCache';
	import {
		createLibraryController,
		type LibraryAdapter,
		type SidebarData
	} from '$lib/utils/libraryController.svelte';
	import LibraryPage from '$lib/components/LibraryPage.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import type { NavidromeAlbumSummary, NavidromeLibraryStats } from '$lib/types';
	import { authStore } from '$lib/stores/authStore.svelte';

	const userId = authStore.user?.id ?? '';
	const foldersQuery = getRemoteFoldersQuery(() => Boolean(userId));
	// The resolved folder selection scopes the cached album lists: a changed
	// selection reads as a new scope.
	const scopeRevision = $derived(
		foldersQuery.data
			? `${foldersQuery.data.mode}:${[...foldersQuery.data.folder_ids].sort().join(',')}`
			: 'unresolved'
	);
	$effect(() => {
		if (foldersQuery.data) setNavidromeFolderScopeRevision(userId, scopeRevision);
	});

	async function fetchAlbumTracks(albumId: string) {
		const page = await remoteApi.albumTracks('navidrome', albumId, { limit: 500 });
		return page.items.map(toNavidromeTrack);
	}

	const adapter: LibraryAdapter<NavidromeAlbumSummary> = {
		sourceType: 'navidrome',

		getAlbumId: (a) => a.navidrome_id,
		getAlbumName: (a) => a.name,
		getArtistName: (a) => a.artist_name,
		getAlbumMbid: (a) => a.musicbrainz_id ?? undefined,
		getAlbumImageUrl: (a) => a.image_url ?? null,
		getAlbumYear: (a) => a.year,

		async fetchAlbums({ limit, offset, sortBy, sortOrder, genre, search, signal }) {
			if (search) {
				const data = await api.v3.GET(REMOTE_ENDPOINTS.search('navidrome', { q: search }), {
					signal
				});
				const items = data.albums.map(toNavidromeAlbum);
				return { items, total: items.length };
			}
			const data = await remoteApi.albums(
				'navidrome',
				{
					limit,
					offset,
					sort_by: sortBy,
					sort_order: sortOrder,
					genre
				},
				signal
			);
			return { items: data.items.map(toNavidromeAlbum), total: data.total };
		},

		async fetchSidebarData(signal, current) {
			const [recentRes, favRes, genreRes, statsRes] = await Promise.allSettled([
				remoteApi.recent('navidrome', {}, signal),
				api.v3.GET(REMOTE_ENDPOINTS.favorites('navidrome'), { signal }),
				remoteApi.genres('navidrome', signal),
				remoteApi.stats('navidrome', signal)
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
							? recentRes.value.map(toNavidromeAlbum)
							: current.recentAlbums,
					favoriteAlbums:
						favRes.status === 'fulfilled'
							? favRes.value.albums.map(toNavidromeAlbum)
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
			const tracks = await fetchAlbumTracks(album.navidrome_id);
			if (tracks.length === 0) return [];
			const sorted = [...tracks].sort((a, b) => a.track_number - b.track_number);
			return buildQueueItemsFromNavidrome(sorted, {
				albumId: album.musicbrainz_id || album.navidrome_id,
				albumName: album.name,
				artistName: album.artist_name,
				coverUrl: album.image_url ?? null,
				artistId: album.artist_musicbrainz_id ?? undefined
			});
		},

		async launchPlayback(album, shuffle) {
			const tracks = await fetchAlbumTracks(album.navidrome_id);
			if (tracks.length === 0) return;
			launchNavidromePlayback(tracks, 0, shuffle, {
				albumId: album.musicbrainz_id || album.navidrome_id,
				albumName: album.name,
				artistName: album.artist_name,
				coverUrl: getCoverUrl(album.image_url ?? null, album.musicbrainz_id || album.navidrome_id)
			});
		},

		getAlbumsListCached: (key) => getNavidromeAlbumsListCachedData(userId, scopeRevision, key),
		setAlbumsListCached: (key, data) =>
			setNavidromeAlbumsListCachedData(data, userId, scopeRevision, key),
		isAlbumsListCacheStale: (ts) => isNavidromeAlbumsListCacheStale(ts),
		getSidebarCached: () => {
			const c = getNavidromeSidebarCachedData(userId, scopeRevision);
			if (!c) return null;
			return {
				data: {
					...c.data,
					favoriteAlbums: c.data.favoriteAlbums ?? [],
					genres: c.data.genres ?? [],
					moods: []
				} as SidebarData<NavidromeAlbumSummary>,
				timestamp: c.timestamp
			};
		},
		setSidebarCached: (data) =>
			setNavidromeSidebarCachedData(
				{
					recentAlbums: data.recentAlbums,
					favoriteAlbums: data.favoriteAlbums,
					genres: data.genres,
					stats: data.stats as NavidromeLibraryStats | null
				},
				userId,
				scopeRevision
			),
		isSidebarCacheStale: (ts) => isNavidromeSidebarCacheStale(ts),

		sortOptions: [
			{ value: 'name', label: 'Name' },
			{ value: 'date_added', label: 'Date Added' },
			{ value: 'year', label: 'Year' }
		],
		defaultSortBy: 'name',
		ascValue: 'asc',
		descValue: 'desc',
		getDefaultSortOrder: (field) => (field === 'name' ? 'asc' : 'desc'),
		supportsGenres: true,
		supportsMoods: false,
		supportsDecades: false,
		supportsTags: false,
		supportsFavorites: true,
		supportsShuffle: true,
		errorMessage: "Couldn't connect to Navidrome."
	};

	const ctrl = createLibraryController(adapter);

	const urlSearch = new URLSearchParams(window.location.search).get('search');
	if (urlSearch) {
		ctrl.searchQuery = urlSearch;
	}
</script>

<LibraryPage
	{ctrl}
	headerTitle="Navidrome Library"
	backHref="/library/navidrome"
	contextMenuBackdrop
	emptyTitle="No albums found"
	emptyDescription="Make sure Navidrome is set up and has at least one music library."
>
	{#snippet headerIcon()}
		<span style="color: rgb(var(--brand-navidrome));">
			<NavidromeIcon class="h-8 w-8" />
		</span>
	{/snippet}

	{#snippet cardTopLeftBadge(album)}
		<div class="badge badge-sm gap-1 badge-primary">
			<NavidromeIcon class="h-3 w-3" />
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
		<NavidromeIcon class="h-12 w-12 opacity-20" />
	{/snippet}
</LibraryPage>
