<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import ArtistCardSkeleton from '$lib/components/ArtistCardSkeleton.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import LocalIdentityBadge from '$lib/components/library/LocalIdentityBadge.svelte';
	import { getLibraryArtistsInfiniteQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import type { ArtistSort, LibraryArtistScope, LibraryArtistSummary } from '$lib/types';
	import { artistHref } from '$lib/utils/entityRoutes';
	import { ChevronLeft, Disc3, Mic, Search, UsersRound, X } from 'lucide-svelte';

	const SEARCH_DEBOUNCE_MS = 300;

	const browse = $derived.by(() => {
		const view = page.url.searchParams.get('view') === 'contributors' ? 'contributors' : 'albums';
		const scope: LibraryArtistScope = view === 'contributors' ? 'contributors' : 'album';
		const rawSort = page.url.searchParams.get('sort') as ArtistSort | null;
		const validSorts: ArtistSort[] =
			scope === 'contributors'
				? ['name', 'appearance_count', 'date_added']
				: ['name', 'album_count', 'date_added'];
		return {
			view,
			scope,
			q: page.url.searchParams.get('q') ?? '',
			sortBy: rawSort && validSorts.includes(rawSort) ? rawSort : 'name',
			sortOrder: page.url.searchParams.get('order') === 'desc' ? 'desc' : 'asc'
		} as const;
	});

	const params = $derived({
		sortBy: browse.sortBy,
		sortOrder: browse.sortOrder,
		q: browse.q,
		scope: browse.scope
	});
	const artistsQuery = getLibraryArtistsInfiniteQuery(() => params);

	const artists = $derived.by(() => {
		const seen: Record<string, true> = Object.create(null);
		const out: LibraryArtistSummary[] = [];
		for (const response of artistsQuery.data?.pages ?? []) {
			for (const item of response.items) {
				if (seen[item.id]) continue;
				seen[item.id] = true;
				out.push(item);
			}
		}
		return out;
	});
	const response = $derived(artistsQuery.data?.pages[0]);
	const total = $derived(response?.total ?? 0);
	const albumArtistTotal = $derived(response?.album_artist_total ?? 0);
	const contributorTotal = $derived(response?.contributor_total ?? 0);

	let searchInput = $derived(browse.q);
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;
	$effect(() => () => clearTimeout(searchTimeout));

	function setParams(updates: Record<string, string | null>): void {
		const url = new URL(page.url);
		for (const [key, value] of Object.entries(updates)) {
			if (!value) url.searchParams.delete(key);
			else url.searchParams.set(key, value);
		}
		void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
	}

	function selectView(view: 'albums' | 'contributors'): void {
		setParams({ view: view === 'albums' ? null : view, sort: null, order: null });
	}

	function handleSearchInput(event: Event): void {
		searchInput = (event.target as HTMLInputElement).value;
		clearTimeout(searchTimeout);
		searchTimeout = setTimeout(
			() => setParams({ q: searchInput.trim() || null }),
			SEARCH_DEBOUNCE_MS
		);
	}

	function clearSearch(): void {
		searchInput = '';
		clearTimeout(searchTimeout);
		setParams({ q: null });
	}

	function handleSortChange(event: Event): void {
		const [sort, order] = (event.target as HTMLSelectElement).value.split(':') as [
			ArtistSort,
			'asc' | 'desc'
		];
		setParams({ sort: sort === 'name' ? null : sort, order: order === 'asc' ? null : order });
	}
</script>

<svelte:head><title>Artists · Library</title></svelte:head>

<main class="container mx-auto p-4 md:p-6 lg:p-8">
	<header class="mb-6 flex items-center gap-4">
		<button
			class="btn btn-ghost btn-circle"
			onclick={() => goto(resolve('/library'))}
			aria-label="Back to library"
		>
			<ChevronLeft class="h-6 w-6" />
		</button>
		<div>
			<p class="text-xs font-semibold uppercase tracking-[0.18em] text-primary">Your collection</p>
			<h1 class="text-3xl font-black tracking-tight">Artists</h1>
			<p class="mt-1 text-sm text-base-content/60">
				{browse.scope === 'album'
					? 'The artists who own releases in your library.'
					: 'Guests, featured artists, and track-level credits.'}
			</p>
		</div>
	</header>

	<nav
		class="mb-6 grid overflow-hidden rounded-2xl border border-base-content/10 bg-base-200/45 p-1 sm:grid-cols-2"
		aria-label="Artist library views"
	>
		<button
			class="group flex items-center gap-3 rounded-xl px-4 py-3 text-left transition-colors"
			class:bg-base-100={browse.view === 'albums'}
			class:shadow-sm={browse.view === 'albums'}
			onclick={() => selectView('albums')}
			aria-current={browse.view === 'albums' ? 'page' : undefined}
		>
			<span class="rounded-xl bg-primary/10 p-2 text-primary"><Disc3 class="h-5 w-5" /></span>
			<span class="min-w-0 flex-1">
				<span class="block font-bold">Album artists</span>
				<span class="block truncate text-xs text-base-content/55">Release-level ownership</span>
			</span>
			<span class="badge badge-ghost tabular-nums">{albumArtistTotal}</span>
		</button>
		<button
			class="group flex items-center gap-3 rounded-xl px-4 py-3 text-left transition-colors"
			class:bg-base-100={browse.view === 'contributors'}
			class:shadow-sm={browse.view === 'contributors'}
			onclick={() => selectView('contributors')}
			aria-current={browse.view === 'contributors' ? 'page' : undefined}
		>
			<span class="rounded-xl bg-secondary/10 p-2 text-secondary"
				><UsersRound class="h-5 w-5" /></span
			>
			<span class="min-w-0 flex-1">
				<span class="block font-bold">Contributors</span>
				<span class="block truncate text-xs text-base-content/55">Track-level appearances</span>
			</span>
			<span class="badge badge-ghost tabular-nums">{contributorTotal}</span>
		</button>
	</nav>

	<div class="mb-6 flex flex-col gap-3 sm:flex-row">
		<div class="group relative flex-1">
			<Search
				class="pointer-events-none absolute left-4 top-1/2 h-5 w-5 -translate-y-1/2 text-base-content/40 transition-colors duration-200 group-focus-within:text-primary"
			/>
			<input
				type="text"
				placeholder={browse.scope === 'album' ? 'Search album artists…' : 'Search contributors…'}
				class="input input-bordered w-full rounded-full pl-11 pr-12"
				value={searchInput}
				oninput={handleSearchInput}
				aria-label="Search artists"
			/>
			{#if searchInput}
				<button
					class="btn btn-ghost btn-circle btn-sm absolute right-3 top-1/2 -translate-y-1/2"
					onclick={clearSearch}
					aria-label="Clear search"
				>
					<X class="h-4 w-4" />
				</button>
			{/if}
		</div>
		<select
			class="select select-bordered rounded-full"
			value="{browse.sortBy}:{browse.sortOrder}"
			onchange={handleSortChange}
			aria-label="Sort artists"
		>
			<option value="name:asc">Name A-Z</option>
			<option value="name:desc">Name Z-A</option>
			{#if browse.scope === 'album'}
				<option value="album_count:desc">Most releases</option>
				<option value="album_count:asc">Fewest releases</option>
			{:else}
				<option value="appearance_count:desc">Most appearances</option>
				<option value="appearance_count:asc">Fewest appearances</option>
			{/if}
			<option value="date_added:desc">Newest first</option>
			<option value="date_added:asc">Oldest first</option>
		</select>
	</div>

	{#if artistsQuery.isError}
		<div class="alert alert-error mb-6">
			<span>Couldn't load artists.</span>
			<button class="btn btn-ghost btn-sm" onclick={() => artistsQuery.refetch()}>Retry</button>
		</div>
	{:else if artistsQuery.isLoading}
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
			{#each Array(12) as _, index (`skeleton-${index}`)}<ArtistCardSkeleton />{/each}
		</div>
	{:else if artists.length === 0}
		<div class="flex min-h-100 flex-col items-center justify-center text-center">
			<Mic class="mb-4 h-12 w-12 text-base-content/35" strokeWidth={1.5} />
			<h2 class="mb-2 text-2xl font-semibold">
				{browse.q ? 'No matching artists' : 'Nothing in this view yet'}
			</h2>
			<p class="max-w-md text-base-content/65">
				{browse.q
					? 'Try a different search term.'
					: browse.scope === 'album'
						? 'Album artists appear here as releases are added to your library.'
						: 'Track-level guests and featured artists will appear here.'}
			</p>
		</div>
	{:else}
		<p class="mb-3 text-sm text-base-content/55">
			{total}
			{total === 1 ? 'artist' : 'artists'} in this view
		</p>
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
			{#each artists as artist (artist.id)}
				<a
					href={artistHref(artist.musicbrainz_artist_id ?? artist.id)}
					class="card group overflow-hidden border border-base-content/8 bg-base-100 shadow-sm transition-[transform,box-shadow,border-color] hover:-translate-y-1 hover:border-primary/25 hover:shadow-lg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary"
					aria-label={`Open ${artist.name}`}
				>
					<figure class="relative aspect-square overflow-hidden p-3">
						<ArtistImage
							mbid={artist.id}
							source="local"
							alt={artist.name}
							size="full"
							requestSize={250}
							className="h-full w-full transition-transform duration-300 group-hover:scale-105"
						/>
						{#if artist.artist_identity_state === 'local_only'}
							<LocalIdentityBadge
								state={artist.artist_identity_state}
								subject="artist"
								compact
								className="absolute left-3 top-3 z-10"
							/>
						{/if}
					</figure>
					<div class="card-body gap-1 p-3 pt-0 text-center">
						<h2 class="truncate font-semibold">{artist.name}</h2>
						<p class="text-xs text-base-content/55">
							{#if browse.scope === 'album'}
								{artist.album_count}
								{artist.album_count === 1 ? 'release' : 'releases'} ·
								{artist.track_count}
								{artist.track_count === 1 ? 'track' : 'tracks'}
							{:else}
								{artist.appearance_release_count}
								{artist.appearance_release_count === 1 ? 'release' : 'releases'} ·
								{artist.appearance_track_count}
								{artist.appearance_track_count === 1 ? 'appearance' : 'appearances'}
							{/if}
						</p>
					</div>
				</a>
			{/each}
		</div>
		{#if artistsQuery.hasNextPage}
			<div class="mt-6 flex justify-center">
				<button
					class="btn btn-primary btn-outline"
					onclick={() => artistsQuery.fetchNextPage()}
					disabled={artistsQuery.isFetchingNextPage}
				>
					{#if artistsQuery.isFetchingNextPage}<span class="loading loading-spinner loading-sm"
						></span>{/if}
					Load more ({artists.length} / {total})
				</button>
			</div>
		{/if}
	{/if}
</main>

<style>
	@media (prefers-reduced-motion: reduce) {
		.card,
		.card :global(img) {
			transition: none;
		}
	}
</style>
