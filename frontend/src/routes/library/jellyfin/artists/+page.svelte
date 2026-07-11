<script lang="ts">
	import { resolve } from '$app/paths';
	import { API } from '$lib/constants';
	import { api } from '$lib/api/client';
	import SourceArtistCard from '$lib/components/SourceArtistCard.svelte';
	import ArtistCardSkeleton from '$lib/components/ArtistCardSkeleton.svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import { artistHrefOrNull } from '$lib/utils/entityRoutes';
	import { reveal } from '$lib/actions/reveal';
	import LibraryFilterBar from '$lib/components/LibraryFilterBar.svelte';
	import { ChevronLeft, ChevronRight } from 'lucide-svelte';
	import type { JellyfinArtistPage } from '$lib/types';

	const PAGE_SIZE = 48;

	let loading = $state(true);
	let artists = $state<JellyfinArtistPage>({ items: [], total: 0, offset: 0, limit: PAGE_SIZE });
	let currentPage = $state(0);
	let sortBy = $state('SortName');
	let sortOrder = $state('Ascending');
	let searchQuery = $state('');
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;

	const ARTIST_SORT_OPTIONS = [
		{ value: 'SortName', label: 'Name' },
		{ value: 'DateCreated', label: 'Recently Added' },
		{ value: 'DatePlayed', label: 'Recently Played' },
		{ value: 'PlayCount', label: 'Most Played' }
	];

	const totalPages = $derived(Math.ceil(artists.total / PAGE_SIZE));

	async function fetchArtists() {
		loading = true;
		try {
			artists = await api.get<JellyfinArtistPage>(
				API.jellyfinLibrary.artistsBrowse(
					PAGE_SIZE,
					currentPage * PAGE_SIZE,
					sortBy,
					sortOrder,
					searchQuery
				)
			);
		} catch {
			artists = { items: [], total: 0, offset: 0, limit: PAGE_SIZE };
		} finally {
			loading = false;
		}
	}

	function goToPage(page: number) {
		currentPage = page;
		fetchArtists();
		window.scrollTo({ top: 0, behavior: 'smooth' });
	}

	function handleSortChange(value: string) {
		if (value !== sortBy) {
			sortBy = value;
			sortOrder = value === 'SortName' ? 'Ascending' : 'Descending';
		}
		currentPage = 0;
		fetchArtists();
	}

	function toggleSortOrder() {
		sortOrder = sortOrder === 'Ascending' ? 'Descending' : 'Ascending';
		currentPage = 0;
		fetchArtists();
	}

	function handleSearchInput() {
		clearTimeout(searchTimeout);
		searchTimeout = setTimeout(() => {
			currentPage = 0;
			fetchArtists();
		}, 300);
	}

	$effect(() => {
		fetchArtists();
	});
</script>

<div class="container mx-auto max-w-7xl px-4 py-6">
	<div
		class="h-[2px] rounded-full bg-gradient-to-r from-transparent via-[rgb(var(--brand-jellyfin))] to-transparent opacity-40 mb-6"
	></div>

	<div
		class="mb-6 rounded-xl bg-base-200/30 backdrop-blur-sm border border-base-content/5 px-5 py-4 shadow-sm flex items-center gap-3"
	>
		<a
			href={resolve('/library/jellyfin')}
			class="btn btn-ghost btn-sm gap-1"
			aria-label="Back to Jellyfin library"
		>
			<ChevronLeft class="h-4 w-4" />
			Back
		</a>
		<JellyfinIcon class="h-6 w-6" />
		<h1 class="text-2xl font-bold">Jellyfin Artists</h1>
		{#if artists.total > 0}
			<span class="badge badge-ghost">{artists.total}</span>
		{/if}
	</div>

	<LibraryFilterBar
		bind:searchQuery
		onSearchInput={handleSearchInput}
		placeholder="Search artists..."
		ariaLabel="Search artists"
		sortOptions={ARTIST_SORT_OPTIONS}
		{sortBy}
		onSortChange={handleSortChange}
		{sortOrder}
		onToggleSortOrder={toggleSortOrder}
		ascValue="Ascending"
		resultCount={loading ? null : artists.total}
		{loading}
	/>

	{#if loading}
		<div class="rounded-xl bg-base-100/40 p-5 shadow-sm">
			<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6">
				{#each Array(PAGE_SIZE) as _, i (i)}
					<ArtistCardSkeleton />
				{/each}
			</div>
		</div>
	{:else if artists.items.length === 0}
		<div class="flex flex-col items-center justify-center py-20 text-base-content/50">
			<JellyfinIcon class="mb-4 h-12 w-12 opacity-20" />
			<p class="text-lg font-medium">{searchQuery ? 'No results found' : 'No artists found'}</p>
			<p class="mt-1 text-sm">
				{searchQuery
					? `Try a different search term`
					: 'Make sure Jellyfin has music in its library'}
			</p>
		</div>
	{:else}
		<div class="rounded-xl bg-base-100/40 p-5 shadow-sm" use:reveal>
			<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6">
				{#each artists.items as artist (artist.jellyfin_id)}
					<SourceArtistCard
						name={artist.name}
						imageUrl={artist.image_url}
						mbid={artist.musicbrainz_id}
						href={artistHrefOrNull(artist.musicbrainz_id) ?? undefined}
						albumCount={artist.album_count}
					/>
				{/each}
			</div>
		</div>

		{#if totalPages > 1}
			<div class="mt-8 flex items-center justify-center gap-2">
				<button
					class="btn btn-ghost btn-sm"
					disabled={currentPage === 0}
					onclick={() => goToPage(currentPage - 1)}
					aria-label="Previous page"
				>
					<ChevronLeft class="h-4 w-4" />
				</button>
				<span class="text-sm text-base-content/70">
					Page {currentPage + 1} of {totalPages}
				</span>
				<button
					class="btn btn-ghost btn-sm"
					disabled={currentPage >= totalPages - 1}
					onclick={() => goToPage(currentPage + 1)}
					aria-label="Next page"
				>
					<ChevronRight class="h-4 w-4" />
				</button>
			</div>
		{/if}
	{/if}
</div>
