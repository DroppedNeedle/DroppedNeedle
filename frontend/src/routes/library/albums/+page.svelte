<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import { ChevronLeft, Disc3, Search, X } from 'lucide-svelte';
	import { getLibraryAlbumsInfiniteQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import type { LibraryV3AlbumsParams } from '$lib/queries/library/LibraryQueryKeyFactory';
	import LibraryAlbumCard from '$lib/components/library/LibraryAlbumCard.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import type { AlbumSort, LibraryAlbumSummary } from '$lib/types';

	const PAGE_SIZE = 50;
	const SEARCH_DEBOUNCE_MS = 300;
	const VALID_SORTS: AlbumSort[] = ['recent', 'title', 'artist'];
	const FORMATS = ['flac', 'mp3', 'm4a', 'opus', 'ogg'];

	// browse state lives in the url so it's shareable and back-button stable
	const params = $derived.by(() => {
		const sp = page.url.searchParams;
		const rawSort = (sp.get('sort') ?? 'recent') as AlbumSort;
		return {
			sort: VALID_SORTS.includes(rawSort) ? rawSort : 'recent',
			q: sp.get('q') ?? '',
			format: sp.get('format') ?? ''
		};
	});

	// The url keeps the v1 sort vocabulary so shared links survive; it maps
	// onto the v3 catalog sorts one row below.
	const SORT_MAP: Record<AlbumSort, Pick<LibraryV3AlbumsParams, 'sort' | 'order'>> = {
		recent: { sort: 'date_added', order: 'desc' },
		title: { sort: 'name', order: 'asc' },
		artist: { sort: 'artist', order: 'asc' }
	};

	const albumsQuery = getLibraryAlbumsInfiniteQuery(() => ({
		limit: PAGE_SIZE,
		...SORT_MAP[params.sort],
		...(params.q ? { q: params.q } : {}),
		...(params.format ? { format: params.format } : {})
	}));
	// An album can move between pages while they load (a scan renames it):
	// keep its first appearance.
	const albums = $derived.by(() => {
		const seen: Record<string, true> = Object.create(null);
		const out: LibraryAlbumSummary[] = [];
		for (const response of albumsQuery.data?.pages ?? []) {
			for (const album of response.items) {
				if (seen[album.id]) continue;
				seen[album.id] = true;
				out.push(album);
			}
		}
		return out;
	});
	const total = $derived(albumsQuery.data?.pages[0]?.total ?? 0);

	// local edits win over the url term until back/forward navigation re-syncs the input
	let searchInput = $derived(params.q);
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;
	$effect(() => () => clearTimeout(searchTimeout));

	function setParams(updates: Record<string, string | null>) {
		const url = new URL(page.url);
		for (const [k, v] of Object.entries(updates)) {
			if (v === null || v === '') url.searchParams.delete(k);
			else url.searchParams.set(k, String(v));
		}
		goto(url, { replaceState: true, keepFocus: true, noScroll: true });
	}

	function handleSearchInput(e: Event) {
		searchInput = (e.target as HTMLInputElement).value;
		clearTimeout(searchTimeout);
		searchTimeout = setTimeout(() => setParams({ q: searchInput.trim() }), SEARCH_DEBOUNCE_MS);
	}

	function clearSearch() {
		searchInput = '';
		clearTimeout(searchTimeout);
		setParams({ q: null });
	}
</script>

<svelte:head><title>Albums · Library</title></svelte:head>

<div class="container mx-auto p-4 md:p-6 lg:p-8">
	<div class="flex items-center gap-4 mb-6">
		<IconButton
			icon={ChevronLeft}
			label="Back to library"
			onclick={() => goto(withBasePath('/library'))}
		/>
		<div>
			<h1 class="text-3xl font-bold">All Albums</h1>
			<p class="text-base-content/70 text-sm mt-1">
				{total}
				{total === 1 ? 'album' : 'albums'}
			</p>
		</div>
	</div>

	<div class="flex flex-col sm:flex-row gap-3 mb-6">
		<div class="relative flex-1">
			<Input
				icon={Search}
				label="Search albums"
				placeholder="Search albums or artists..."
				class="w-full pr-12"
				value={searchInput}
				oninput={handleSearchInput}
			/>
			{#if searchInput}
				<IconButton
					size="sm"
					icon={X}
					label="Clear search"
					class="absolute right-2 top-1/2 -translate-y-1/2"
					onclick={clearSearch}
				/>
			{/if}
		</div>
		<select
			class="select"
			value={params.sort}
			onchange={(e) => setParams({ sort: (e.target as HTMLSelectElement).value })}
			aria-label="Sort albums"
		>
			<option value="recent">Newest First</option>
			<option value="title">Title A-Z</option>
			<option value="artist">Artist A-Z</option>
		</select>
		<select
			class="select"
			value={params.format}
			onchange={(e) => setParams({ format: (e.target as HTMLSelectElement).value })}
			aria-label="Filter by format"
		>
			<option value="">All formats</option>
			{#each FORMATS as f (f)}
				<option value={f}>{f.toUpperCase()}</option>
			{/each}
		</select>
	</div>

	{#if albumsQuery.isError}
		<div class="alert alert-error mb-6">
			<span>Couldn't load albums</span>
			<Button variant="ghost" size="sm" onclick={() => albumsQuery.refetch()}>Retry</Button>
		</div>
	{:else if albumsQuery.isLoading}
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
			{#each Array(12) as _, i (`skeleton-${i}`)}
				<div class="skeleton aspect-square w-full rounded-lg"></div>
			{/each}
		</div>
	{:else if albums.length === 0}
		<div class="flex flex-col items-center justify-center min-h-100 text-center">
			<Disc3 class="h-12 w-12 text-base-content/40 mb-4" strokeWidth={1.5} />
			<h2 class="text-2xl font-semibold mb-2">No albums found</h2>
			<p class="text-base-content/70 mb-4">
				{params.q || params.format
					? 'Try a different search or filter.'
					: "Your library doesn't contain any albums yet."}
			</p>
		</div>
	{:else}
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
			{#each albums as album (album.id)}
				<LibraryAlbumCard {album} />
			{/each}
		</div>

		{#if albumsQuery.hasNextPage}
			<div class="mt-6 flex justify-center">
				<Button
					variant="outline"
					loading={albumsQuery.isFetchingNextPage}
					onclick={() => albumsQuery.fetchNextPage()}
				>
					Load more ({albums.length} / {total})
				</Button>
			</div>
		{/if}
	{/if}
</div>
