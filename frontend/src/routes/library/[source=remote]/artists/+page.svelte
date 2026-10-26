<script lang="ts">
	import { page } from '$app/state';
	import { untrack } from 'svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { getRemoteArtistsQuery } from '$lib/queries/remotes/RemoteQueries.svelte';
	import { defaultSortOrder } from '$lib/queries/remotes/remoteSources';
	import SourceArtistCard from '$lib/components/SourceArtistCard.svelte';
	import ArtistCardSkeleton from '$lib/components/ArtistCardSkeleton.svelte';
	import LibraryFilterBar from '$lib/components/LibraryFilterBar.svelte';
	import { artistHrefOrNull } from '$lib/utils/entityRoutes';
	import { reveal } from '$lib/actions/reveal';
	import ListHeader from '../ListHeader.svelte';
	import Pager from '../Pager.svelte';
	import type { PageData } from './$types';

	const PAGE_SIZE = 48;

	let { data }: { data: PageData } = $props();
	const remote = $derived(data.remote);
	// The layout remounts the page for another server, so its first sort stands.
	const start = untrack(() => data.remote);
	const sorts = $derived(remote.sorts.artists);

	const initialSearch = page.url.searchParams.get('search') ?? '';
	let currentPage = $state(0);
	let sortBy = $state(start.sorts.artists?.[0]?.value ?? '');
	let sortOrder = $state<'asc' | 'desc'>('asc');
	let searchQuery = $state(initialSearch);
	let search = $state(initialSearch);
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;

	const artistsQuery = getRemoteArtistsQuery(
		() => remote.source,
		() => ({
			limit: PAGE_SIZE,
			offset: currentPage * PAGE_SIZE,
			search,
			...(sorts ? { sort_by: sortBy, sort_order: sortOrder } : {})
		})
	);
	const total = $derived(artistsQuery.data?.total ?? 0);
	const totalPages = $derived(Math.ceil(total / PAGE_SIZE));

	function goToPage(next: number) {
		currentPage = next;
		window.scrollTo({ top: 0, behavior: 'smooth' });
	}

	function handleSortChange(value: string) {
		if (value !== sortBy && sorts) {
			sortBy = value;
			sortOrder = defaultSortOrder(sorts, value);
		}
		currentPage = 0;
	}

	function toggleSortOrder() {
		sortOrder = sortOrder === 'asc' ? 'desc' : 'asc';
		currentPage = 0;
	}

	function handleSearchInput() {
		clearTimeout(searchTimeout);
		searchTimeout = setTimeout(() => {
			currentPage = 0;
			search = searchQuery;
		}, 300);
	}
</script>

<div class="container mx-auto max-w-7xl px-4 py-6">
	<ListHeader {remote} kind="Artists" {total} />

	<LibraryFilterBar
		bind:searchQuery
		onSearchInput={handleSearchInput}
		placeholder="Search artists..."
		ariaLabel="Search artists"
		sortOptions={sorts ?? undefined}
		sortBy={sorts ? sortBy : undefined}
		onSortChange={sorts ? handleSortChange : undefined}
		sortOrder={sorts ? sortOrder : undefined}
		onToggleSortOrder={sorts ? toggleSortOrder : undefined}
		ascValue="asc"
		resultCount={artistsQuery.isPending ? null : total}
		loading={artistsQuery.isPending}
	/>

	<QueryBoundary query={artistsQuery} what="{remote.label} artists">
		{#snippet loading()}
			<div class="rounded-xl bg-base-100/40 p-5 shadow-sm">
				<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6">
					{#each Array(PAGE_SIZE) as _, i (i)}
						<ArtistCardSkeleton />
					{/each}
				</div>
			</div>
		{/snippet}
		{#snippet empty()}
			<div class="flex flex-col items-center justify-center py-20 text-base-content/50">
				<remote.icon class="mb-4 h-12 w-12 opacity-20" />
				<p class="text-lg font-medium">{search ? 'No results found' : 'No artists found'}</p>
				<p class="mt-1 text-sm">
					{search
						? 'Try a different search term'
						: `Make sure ${remote.label} has music in its library`}
				</p>
			</div>
		{/snippet}
		{#snippet children(artists)}
			<div class="rounded-xl bg-base-100/40 p-5 shadow-sm" use:reveal>
				<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6">
					{#each artists.items as artist (artist.id)}
						<SourceArtistCard
							name={artist.name}
							imageUrl={artist.image_url}
							mbid={artist.artist_mbid}
							href={artistHrefOrNull(artist.artist_mbid) ?? undefined}
							albumCount={artist.album_count ?? undefined}
						/>
					{/each}
				</div>
			</div>

			<Pager page={currentPage} pages={totalPages} ongo={goToPage} />
		{/snippet}
	</QueryBoundary>
</div>
