<script lang="ts">
	import { page } from '$app/state';
	import { untrack } from 'svelte';
	import {
		fetchRemoteAlbumTracks,
		getRemoteAlbumsInfiniteQuery,
		getRemoteFavoritesQuery,
		getRemoteFiltersQuery,
		getRemoteFoldersQuery,
		getRemoteGenresQuery,
		getRemoteMoodsQuery,
		getRemoteRecentQuery,
		getRemoteStatsQuery
	} from '$lib/queries/remotes/RemoteQueries.svelte';
	import { defaultSortOrder } from '$lib/queries/remotes/remoteSources';
	import type { RemoteAlbum } from '$lib/queries/remotes/types';
	import type { PlaybackMeta, QueueItem } from '$lib/player/types';
	import { playerStore } from '$lib/stores/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import AddToPlaylistModal from '$lib/components/AddToPlaylistModal.svelte';
	import SourceAlbumModal from '$lib/components/SourceAlbumModal.svelte';
	import LibraryFilterBar from '$lib/components/LibraryFilterBar.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { withBasePath } from '$lib/utils/basePath';
	import { ChevronLeft, ListMusic, ListPlus, ListStart, Play, Shuffle } from 'lucide-svelte';
	import type { PageData } from './$types';

	const DECADES = ['2020s', '2010s', '2000s', '1990s', '1980s', '1970s', '1960s'];

	let { data }: { data: PageData } = $props();
	const remote = $derived(data.remote);
	// The layout remounts the page for another server, so its first sort stands.
	const start = untrack(() => data.remote);
	const caps = $derived(remote.capabilities);
	const sorts = $derived(remote.sorts.albums);

	// Deep links: ?sort= picks a sort the server offers, ?search= starts a search.
	const urlSort = page.url.searchParams.get('sort');
	const initialSort =
		urlSort && start.sorts.albums.some((s) => s.value === urlSort)
			? urlSort
			: start.sorts.albums[0].value;
	const initialSearch = page.url.searchParams.get('search') ?? '';

	let sortBy = $state(initialSort);
	let sortOrder = $state(defaultSortOrder(start.sorts.albums, initialSort));
	let genre = $state('');
	let mood = $state('');
	let decade = $state('');
	let tag = $state('');
	let searchQuery = $state(initialSearch);
	let search = $state(initialSearch);
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;

	let selectedAlbum = $state<RemoteAlbum | null>(null);
	let detailOpen = $state(false);
	let playingId = $state<string | null>(null);
	let menuLoadingId = $state<string | null>(null);
	let playlistModal = $state<{ open: (tracks: QueueItem[]) => void } | null>(null);

	// Plex filters by mood and decade; Jellyfin by studio, year and tag from its facets.
	const albumParams = $derived({
		sort_by: sortBy,
		sort_order: sortOrder,
		genre: genre || undefined,
		...(caps.moods ? { mood: mood || undefined } : {}),
		...(caps.facets ? { studios: mood || undefined, tags: tag || undefined } : {}),
		...(caps.decades ? { decade: decade || undefined } : {}),
		...(caps.facets ? { year: decade ? parseInt(decade) || undefined : undefined } : {})
	});

	const albumsQuery = getRemoteAlbumsInfiniteQuery(
		() => remote.source,
		() => albumParams,
		() => search
	);
	const statsQuery = getRemoteStatsQuery(() => remote.source);
	const recentQuery = getRemoteRecentQuery(() => remote.source);
	const genresQuery = getRemoteGenresQuery(() => remote.source);
	const favoritesQuery = getRemoteFavoritesQuery(
		() => remote.source,
		() => caps.favorites
	);
	const moodsQuery = getRemoteMoodsQuery(
		() => remote.source,
		() => caps.moods
	);
	const filtersQuery = getRemoteFiltersQuery(
		() => remote.source,
		() => caps.facets
	);
	// The folder choice keys what is cached for this server elsewhere in the app.
	getRemoteFoldersQuery(() => caps.folders);

	const total = $derived(albumsQuery.data?.pages.at(-1)?.total ?? 0);
	const recentAlbums = $derived(recentQuery.data ?? []);
	const favoriteAlbums = $derived(caps.favorites ? (favoritesQuery.data?.albums ?? []) : []);
	const moodChips = $derived(
		caps.moods ? (moodsQuery.data ?? []) : caps.facets ? filtersQuery.data?.studios : undefined
	);
	const decadeChips = $derived(
		caps.decades ? DECADES : caps.facets ? filtersQuery.data?.years.map(String) : undefined
	);
	const tagChips = $derived(caps.facets ? filtersQuery.data?.tags : undefined);

	const mbid = (album: RemoteAlbum) => album.release_group_mbid ?? undefined;
	const albumKey = (album: RemoteAlbum) => album.release_group_mbid || album.id;
	const meta = (album: RemoteAlbum): PlaybackMeta => ({
		albumId: albumKey(album),
		albumName: album.title,
		artistName: album.artist_name,
		coverUrl: album.image_url ?? null,
		artistId: album.artist_mbid ?? undefined
	});

	function handleSortChange(value: string) {
		if (value !== sortBy) {
			sortBy = value;
			sortOrder = defaultSortOrder(sorts, value);
		}
	}

	function handleSearch() {
		clearTimeout(searchTimeout);
		searchTimeout = setTimeout(() => (search = searchQuery), 300);
	}

	function openDetail(album: RemoteAlbum) {
		selectedAlbum = album;
		detailOpen = true;
	}

	async function quickPlay(album: RemoteAlbum, shuffle: boolean, e: Event) {
		e.stopPropagation();
		playingId = album.id;
		try {
			const tracks = await fetchRemoteAlbumTracks(remote.source, album.id);
			if (tracks.items.length === 0) return;
			remote.playAlbum(tracks.items, shuffle, {
				...meta(album),
				coverUrl: getCoverUrl(album.image_url ?? null, albumKey(album))
			});
		} catch {
			toastStore.show({ message: 'Playback failed', type: 'error' });
		} finally {
			playingId = null;
		}
	}

	async function withAlbumQueue(album: RemoteAlbum, use: (items: QueueItem[]) => void) {
		menuLoadingId = album.id;
		try {
			const tracks = await fetchRemoteAlbumTracks(remote.source, album.id);
			const items = remote.albumQueue(tracks.items, meta(album));
			if (items.length === 0) {
				toastStore.show({ message: 'No tracks found for this album', type: 'info' });
				return;
			}
			use(items);
		} catch {
			toastStore.show({ message: 'Failed to load album tracks', type: 'error' });
		} finally {
			menuLoadingId = null;
		}
	}

	function albumMenu(album: RemoteAlbum): MenuItem[] {
		const disabled = menuLoadingId === album.id;
		return [
			{
				label: 'Add to Queue',
				icon: ListPlus,
				onclick: () => void withAlbumQueue(album, (items) => playerStore.addMultipleToQueue(items)),
				disabled
			},
			{
				label: 'Play Next',
				icon: ListStart,
				onclick: () => void withAlbumQueue(album, (items) => playerStore.playMultipleNext(items)),
				disabled
			},
			{
				label: 'Add to Playlist',
				icon: ListMusic,
				onclick: () => void withAlbumQueue(album, (items) => playlistModal?.open(items)),
				disabled
			}
		];
	}
</script>

{#snippet shelf(title: string, items: RemoteAlbum[])}
	<div class="mb-8">
		<h2 class="text-lg font-semibold mb-3 opacity-80">{title}</h2>
		<div class="flex gap-3 overflow-x-auto pb-2">
			{#each items as album (album.id)}
				<button
					class="shrink-0 w-36 group cursor-pointer transition-transform hover:scale-105 active:scale-95"
					onclick={() => openDetail(album)}
				>
					<div class="aspect-square rounded-lg overflow-hidden shadow-sm relative">
						<AlbumImage
							mbid={mbid(album) ?? album.id}
							customUrl={album.image_url ?? null}
							alt={album.title}
							size="full"
							requestSize={250}
							rounded="none"
							className="w-full h-full"
						/>
					</div>
					<p class="text-sm font-medium mt-1 line-clamp-1">{album.title}</p>
					<p class="text-xs opacity-60 line-clamp-1">{album.artist_name}</p>
				</button>
			{/each}
		</div>
	</div>
{/snippet}

<div class="container mx-auto p-6">
	<div class="flex items-center gap-3 mb-2">
		<IconButton
			size="sm"
			href={withBasePath(`/library/${remote.source}`)}
			icon={ChevronLeft}
			label="Back"
		/>
		<span class={remote.tone}>
			<remote.icon class="h-8 w-8" />
		</span>
		<h1 class="text-2xl font-bold">{remote.label} Library</h1>
		{#if statsQuery.data}
			<span class="badge badge-neutral">{statsQuery.data.total_albums ?? 0} albums</span>
		{/if}
	</div>

	{#if recentAlbums.length > 0}
		{@render shelf('Recently played', recentAlbums)}
	{/if}

	{#if favoriteAlbums.length > 0}
		{@render shelf('Favorites', favoriteAlbums)}
	{/if}

	<div class="mb-6">
		<h2 class="text-lg font-semibold mb-3 opacity-80">All albums</h2>
		<LibraryFilterBar
			bind:searchQuery
			onSearchInput={handleSearch}
			sortOptions={sorts}
			{sortBy}
			onSortChange={handleSortChange}
			{sortOrder}
			onToggleSortOrder={() => (sortOrder = sortOrder === 'asc' ? 'desc' : 'asc')}
			ascValue="asc"
			genres={genresQuery.data ?? []}
			selectedGenre={genre}
			onGenreChange={(value) => (genre = value)}
			moods={moodChips}
			selectedMood={moodChips ? mood : undefined}
			onMoodChange={moodChips ? (value) => (mood = value) : undefined}
			moodLabel={caps.moods ? 'Mood' : 'Studio'}
			decades={decadeChips}
			selectedDecade={decadeChips ? decade : undefined}
			onDecadeChange={decadeChips ? (value) => (decade = value) : undefined}
			tags={tagChips}
			selectedTag={tagChips ? tag : undefined}
			onTagChange={tagChips ? (value) => (tag = value) : undefined}
			resultCount={albumsQuery.isPending ? null : total}
			loading={albumsQuery.isPending}
		/>
	</div>

	<QueryBoundary
		query={albumsQuery}
		what="{remote.label} albums"
		isEmpty={(d) => !d.pages[0]?.items.length}
	>
		{#snippet loading()}
			<div
				class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
			>
				{#each Array(12) as _, i (i)}
					<Card variant="raised" padding="none" class="animate-pulse">
						<div class="aspect-square bg-base-300"></div>
						<div class="p-3">
							<div class="h-4 bg-base-300 rounded w-3/4"></div>
							<div class="h-3 bg-base-300 rounded w-1/2 mt-1"></div>
						</div>
					</Card>
				{/each}
			</div>
		{/snippet}
		{#snippet empty()}
			<Card class="mt-4 text-center">
				<div class="flex justify-center"><remote.icon class="h-12 w-12 opacity-20" /></div>
				<p class="text-lg opacity-60">No albums found</p>
				<p class="text-sm opacity-40">
					Make sure {remote.label} is set up and has at least one music library.
				</p>
			</Card>
		{/snippet}
		{#snippet children(result)}
			<div
				class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
			>
				{#each result.pages.flatMap((p) => p.items) as album (album.id)}
					<Card
						variant="raised"
						padding="none"
						class="group relative w-full cursor-pointer transition hover:shadow-md focus-visible:outline-2 focus-visible:outline-primary motion-safe:hover:-translate-y-0.5"
						onclick={() => openDetail(album)}
						onkeydown={(e) => {
							if (e.target === e.currentTarget && (e.key === 'Enter' || e.key === ' ')) {
								e.preventDefault();
								openDetail(album);
							}
						}}
						role="button"
						tabindex={0}
						aria-label={`Open ${album.title}`}
					>
						<figure class="aspect-square overflow-hidden relative">
							<AlbumImage
								mbid={mbid(album) ?? album.id}
								customUrl={album.image_url ?? null}
								alt={album.title}
								size="full"
								requestSize={250}
								rounded="none"
								className="w-full h-full"
							/>
							<div class="absolute top-2 left-2 flex items-center gap-1">
								<div class="badge badge-sm gap-1 {remote.badge}">
									<remote.icon class="h-3 w-3" />
								</div>
								{#if !album.release_group_mbid}
									<div
										class="badge badge-sm badge-warning gap-1 opacity-80"
										title="Not matched in library - search only"
									>
										?
									</div>
								{/if}
							</div>
							<div class="absolute top-2 right-2 flex items-start gap-1">
								{#if album.year}
									<div class="badge badge-sm badge-ghost">{album.year}</div>
								{/if}
								<div class="rounded-full bg-black/50 backdrop-blur-sm">
									<ContextMenu items={albumMenu(album)} position="end" size="xs" />
								</div>
							</div>
							<div
								class="absolute bottom-2 right-2 flex items-center gap-1.5 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100 transition-opacity"
							>
								<IconButton
									variant="neutral"
									size="sm"
									icon={Shuffle}
									label="Shuffle {album.title}"
									onclick={(e) => quickPlay(album, true, e)}
								/>
								<IconButton
									variant="primary"
									size="sm"
									icon={Play}
									label="Play {album.title}"
									class="[&_svg]:fill-current"
									loading={playingId === album.id}
									onclick={(e) => quickPlay(album, false, e)}
								/>
							</div>
						</figure>

						<div class="flex flex-col gap-2 p-3">
							<CardTitle class="text-sm line-clamp-2 min-h-10">
								{album.title}
							</CardTitle>
							<p class="text-xs opacity-70 line-clamp-1">{album.artist_name}</p>
						</div>
					</Card>
				{/each}
			</div>

			{#if albumsQuery.hasNextPage}
				<div class="flex justify-center mt-6">
					<Button
						variant="ghost"
						loading={albumsQuery.isFetchingNextPage}
						onclick={() => void albumsQuery.fetchNextPage()}>Show more</Button
					>
				</div>
			{/if}
		{/snippet}
	</QueryBoundary>
</div>

<AddToPlaylistModal bind:this={playlistModal} />

<SourceAlbumModal
	bind:open={detailOpen}
	sourceType={remote.source}
	album={selectedAlbum}
	onclose={() => (selectedAlbum = null)}
/>
