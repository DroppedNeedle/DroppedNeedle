<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { ChevronLeft, Disc3, Search, X } from 'lucide-svelte';
	import { getLibraryAlbumsQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import LibraryAlbumCard from '$lib/components/library/LibraryAlbumCard.svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import type { AlbumSort } from '$lib/types';

	const PAGE_SIZE = 50;
	const SEARCH_DEBOUNCE_MS = 300;
	const VALID_SORTS: AlbumSort[] = ['recent', 'title', 'artist'];
	const FORMATS = ['flac', 'mp3', 'm4a', 'opus', 'ogg'];

	// browse state lives in the url so it's shareable and back-button stable
	const params = $derived.by(() => {
		const sp = page.url.searchParams;
		const pageNum = Math.max(1, parseInt(sp.get('page') ?? '1', 10) || 1);
		const rawSort = (sp.get('sort') ?? 'recent') as AlbumSort;
		return {
			page: pageNum,
			sort: VALID_SORTS.includes(rawSort) ? rawSort : 'recent',
			q: sp.get('q') ?? '',
			format: sp.get('format') ?? ''
		};
	});

	const albumsQuery = getLibraryAlbumsQuery(() => params);
	const total = $derived(albumsQuery.data?.total ?? 0);
	const totalPages = $derived(
		albumsQuery.data ? Math.max(1, Math.ceil(albumsQuery.data.total / PAGE_SIZE)) : 1
	);

	// local edits win over the url term until back/forward navigation re-syncs the input
	let searchInput = $derived(params.q);
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;
	$effect(() => () => clearTimeout(searchTimeout));

	function setParams(updates: Record<string, string | number | null>) {
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
		searchTimeout = setTimeout(
			() => setParams({ q: searchInput.trim(), page: null }),
			SEARCH_DEBOUNCE_MS
		);
	}

	function clearSearch() {
		searchInput = '';
		clearTimeout(searchTimeout);
		setParams({ q: null, page: null });
	}
</script>

<svelte:head><title>Albums · Library</title></svelte:head>

<div class="container mx-auto p-4 md:p-6 lg:p-8">
	<div class="flex items-center gap-4 mb-6">
		<button
			class="btn btn-ghost btn-circle"
			onclick={() => goto(resolve('/library'))}
			aria-label="Back to library"
		>
			<ChevronLeft class="w-6 h-6" />
		</button>
		<div>
			<h1 class="text-3xl font-bold">All Albums</h1>
			<p class="text-base-content/70 text-sm mt-1">
				{total}
				{total === 1 ? 'album' : 'albums'}
			</p>
		</div>
	</div>

	<div class="flex flex-col sm:flex-row gap-3 mb-6">
		<div class="relative group flex-1">
			<Search
				class="absolute left-4 top-1/2 -translate-y-1/2 h-5 w-5 text-base-content/40
				group-focus-within:text-primary transition-colors duration-200 pointer-events-none"
			/>
			<input
				type="text"
				placeholder="Search albums or artists..."
				class="input input-bordered w-full rounded-full pl-11 pr-12"
				value={searchInput}
				oninput={handleSearchInput}
				aria-label="Search albums"
			/>
			{#if searchInput}
				<button
					class="absolute right-3 top-1/2 -translate-y-1/2 btn btn-sm btn-ghost btn-circle"
					onclick={clearSearch}
					aria-label="Clear search"
				>
					<X class="h-4 w-4" />
				</button>
			{/if}
		</div>
		<select
			class="select select-bordered rounded-full"
			value={params.sort}
			onchange={(e) => setParams({ sort: (e.target as HTMLSelectElement).value, page: null })}
			aria-label="Sort albums"
		>
			<option value="recent">Newest First</option>
			<option value="title">Title A-Z</option>
			<option value="artist">Artist A-Z</option>
		</select>
		<select
			class="select select-bordered rounded-full"
			value={params.format}
			onchange={(e) => setParams({ format: (e.target as HTMLSelectElement).value, page: null })}
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
			<button class="btn btn-sm btn-ghost" onclick={() => albumsQuery.refetch()}>Retry</button>
		</div>
	{:else if albumsQuery.isLoading}
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
			{#each Array(12) as _, i (`skeleton-${i}`)}
				<div class="skeleton aspect-square w-full rounded-lg"></div>
			{/each}
		</div>
	{:else if !albumsQuery.data || albumsQuery.data.items.length === 0}
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
			{#each albumsQuery.data.items as album (album.id)}
				<LibraryAlbumCard {album} />
			{/each}
		</div>

		{#if totalPages > 1}
			<div class="mt-6 flex justify-center">
				<Pagination
					current={params.page}
					total={totalPages}
					onchange={(p) => setParams({ page: p })}
				/>
			</div>
		{/if}
	{/if}
</div>
