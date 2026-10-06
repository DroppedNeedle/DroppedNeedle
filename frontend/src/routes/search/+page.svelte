<script lang="ts">
	import { onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import AlbumCard from '$lib/components/AlbumCard.svelte';
	import SearchArtistCard from '$lib/components/SearchArtistCard.svelte';
	import ViewMoreAlbumCard from '$lib/components/ViewMoreAlbumCard.svelte';
	import ViewMoreArtistCard from '$lib/components/ViewMoreArtistCard.svelte';
	import ArtistCardSkeleton from '$lib/components/ArtistCardSkeleton.svelte';
	import AlbumCardSkeleton from '$lib/components/AlbumCardSkeleton.svelte';
	import type { EnrichmentSource, SearchRemoteStatus } from '$lib/types';
	import { colors } from '$lib/colors';
	import { applyArtistEnrichment, applyAlbumEnrichment } from '$lib/utils/enrichment';
	import { getSearchStatusNotice } from '$lib/utils/searchStatus';
	import {
		COMBINED_SEARCH_LIMITS,
		getSearchEnrichBatchV3Query,
		getUnifiedSearchV3Query,
		type EnrichmentResponseV3
	} from '$lib/queries/search/SearchV3Queries.svelte';
	import { SearchEnrichCollector } from '$lib/queries/search/SearchV3Enrichment.svelte';
	import {
		dedupeAlbums,
		toSearchRemoteStatus,
		toV1Album,
		toV1Artist
	} from '$lib/queries/search/SearchV3Adapters';
	import { Check, ArrowRight, RefreshCw } from 'lucide-svelte';
	import SearchTopResult from '$lib/components/SearchTopResult.svelte';

	interface Props {
		data: { query: string };
	}

	let { data }: Props = $props();

	let showToast = $state(false);
	let enrichment: EnrichmentResponseV3 | null = $state(null);
	let enrichmentSource: EnrichmentSource = $state('none');
	let enrichmentQuery = $state('');

	let normalizedQuery = $derived(data.query.trim());
	const searchQuery = getUnifiedSearchV3Query(
		() => normalizedQuery,
		() => COMBINED_SEARCH_LIMITS
	);
	const enrichCollector = new SearchEnrichCollector();
	const enrichQuery = getSearchEnrichBatchV3Query(() => enrichCollector.body);

	let remoteArtists = $derived(
		(() => {
			const results = searchQuery.data?.artists ?? [];
			const top = searchQuery.data?.top_artist;
			if (top && !results.some((artist) => artist.id === top.id)) {
				return [top, ...results.slice(0, COMBINED_SEARCH_LIMITS.artists - 1)];
			}
			return results;
		})()
	);
	let baseArtists = $derived(remoteArtists.map(toV1Artist));
	let baseAlbums = $derived(dedupeAlbums((searchQuery.data?.albums ?? []).map(toV1Album)));
	let artists = $derived(enrichment ? applyArtistEnrichment(baseArtists, enrichment) : baseArtists);
	let albums = $derived(enrichment ? applyAlbumEnrichment(baseAlbums, enrichment) : baseAlbums);
	let topArtist = $derived.by(() => {
		const top = searchQuery.data?.top_artist;
		if (!top) return null;
		const id = top.musicbrainz_id ?? top.id;
		return artists.find((artist) => artist.musicbrainz_id === id) ?? null;
	});
	let topAlbum = $derived.by(() => {
		const top = searchQuery.data?.top_album;
		if (!top) return null;
		const id = top.musicbrainz_id ?? top.id;
		return albums.find((album) => album.musicbrainz_id === id) ?? null;
	});
	let artistStatus: SearchRemoteStatus = $derived(
		searchQuery.isError ? 'error' : toSearchRemoteStatus(searchQuery.data?.artist_status ?? 'ok')
	);
	let albumStatus: SearchRemoteStatus = $derived(
		searchQuery.isError ? 'error' : toSearchRemoteStatus(searchQuery.data?.album_status ?? 'ok')
	);
	let artistNotice = $derived(getSearchStatusNotice(artistStatus, 'artists'));
	let albumNotice = $derived(getSearchStatusNotice(albumStatus, 'albums'));
	let loadingArtists = $derived(searchQuery.isPending && artists.length === 0);
	let loadingAlbums = $derived(searchQuery.isPending && albums.length === 0);
	let hasSearched = $derived(normalizedQuery.length >= 2);

	let isSearching = $derived(searchQuery.isFetching);
	let hasTopResult = $derived(topArtist != null || topAlbum != null);
	let displayedArtists = $derived(
		topArtist ? artists.filter((a) => a.musicbrainz_id !== topArtist?.musicbrainz_id) : artists
	);
	let artistCards = $derived(displayedArtists.slice(0, 5));
	let artistPlaceholderCount = $derived(
		searchQuery.isFetching ? Math.max(0, 5 - artistCards.length) : 0
	);
	let displayedAlbums = $derived(
		topAlbum ? albums.filter((a) => a.musicbrainz_id !== topAlbum?.musicbrainz_id) : albums
	);

	function navigateToBucket(bucket: 'artists' | 'albums') {
		if (normalizedQuery) {
			goto(withBasePath(`/search/${bucket}?q=${encodeURIComponent(normalizedQuery)}`));
		}
	}

	function handleAlbumAdded() {
		showToast = true;
		setTimeout(() => {
			showToast = false;
		}, 3000);
	}

	$effect(() => {
		const result = enrichQuery.data;
		if (result) {
			enrichment = result;
			enrichmentSource = result.source;
		}
	});

	$effect(() => {
		if (normalizedQuery === enrichmentQuery) return;
		enrichmentQuery = normalizedQuery;
		enrichCollector.reset();
		enrichment = null;
		enrichmentSource = 'none';
	});

	$effect(() => {
		const handleRefresh = () => {
			void searchQuery.refetch();
		};
		window.addEventListener('search-refresh', handleRefresh);
		return () => window.removeEventListener('search-refresh', handleRefresh);
	});

	onDestroy(() => {
		enrichCollector.dispose();
	});
</script>

{#if hasSearched || isSearching}
	<div class="px-8 pt-4 pb-2">
		<div class="flex gap-2">
			<button
				class="badge badge-lg cursor-pointer"
				style="background-color: {colors.primary}; color: {colors.secondary};"
			>
				All
			</button>
			<button
				class="badge badge-lg cursor-pointer transition-colors"
				style="background-color: {colors.secondary}; color: {colors.primary};"
				onclick={() => navigateToBucket('artists')}
			>
				Artists
			</button>
			<button
				class="badge badge-lg cursor-pointer transition-colors"
				style="background-color: {colors.secondary}; color: {colors.primary};"
				onclick={() => navigateToBucket('albums')}
			>
				Albums
			</button>
		</div>
	</div>
{/if}

{#if hasSearched}
	<section class="px-8 py-4 space-y-8">
		{#if isSearching}
			<div
				class="grid grid-flow-col auto-cols-[85%] gap-3 overflow-x-auto sm:grid-flow-row sm:auto-cols-auto sm:grid-cols-2 sm:overflow-visible"
				aria-label="Loading top search results"
			>
				<div class="skeleton skeleton-shimmer min-h-44 sm:min-h-56 rounded-box"></div>
				<div class="skeleton skeleton-shimmer min-h-44 sm:min-h-56 rounded-box"></div>
			</div>
		{:else if hasTopResult}
			<div
				class="grid grid-flow-col auto-cols-[85%] gap-3 overflow-x-auto sm:grid-flow-row sm:auto-cols-auto sm:grid-cols-2 sm:overflow-visible"
			>
				{#if topArtist}
					<SearchTopResult artist={topArtist} />
				{/if}
				{#if topAlbum}
					<SearchTopResult album={topAlbum} />
				{/if}
			</div>
		{/if}

		<div>
			<h2 class="text-xl font-bold mb-4">Artists</h2>
			{#if artistNotice}
				<div class="alert {artistNotice.className} mb-3" role="status">
					<span>{artistNotice.message}</span>
					<button class="btn btn-sm" onclick={() => searchQuery.refetch()}>
						<RefreshCw class="h-4 w-4" /> Retry
					</button>
				</div>
			{/if}
			{#if loadingArtists}
				<div class="bg-base-200 rounded-box p-4">
					<div
						class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
					>
						{#each Array(6) as _, i (`artist-skeleton-${i}`)}
							<ArtistCardSkeleton variant="detailed" />
						{/each}
					</div>
				</div>
			{:else if displayedArtists.length > 0}
				<div class="bg-base-200 rounded-box p-4 overflow-hidden">
					<div
						class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
						aria-label="Artist search results"
						aria-busy={searchQuery.isFetching}
					>
						<ViewMoreArtistCard />
						{#each artistCards as artist (artist.musicbrainz_id)}
							<SearchArtistCard
								{artist}
								{enrichmentSource}
								onenrichmentrequest={() => enrichCollector.requestArtist(artist)}
							/>
						{/each}
						{#each Array(artistPlaceholderCount) as _, i (`artist-pending-${i}`)}
							<ArtistCardSkeleton variant="detailed" />
						{/each}
					</div>
				</div>
			{:else}
				<div class="p-8 bg-base-200 rounded-box text-center text-gray-500">No artists found</div>
			{/if}
		</div>

		<div>
			<div class="flex items-center justify-between mb-4">
				<h2 class="text-xl font-bold">Albums</h2>
				{#if displayedAlbums.length > 0}
					<a
						href={`/search/albums?q=${encodeURIComponent(normalizedQuery)}`}
						class="text-sm text-primary hover:underline"
					>
						View more <ArrowRight class="h-4 w-4 inline align-middle" />
					</a>
				{/if}
			</div>
			{#if albumNotice}
				<div class="alert {albumNotice.className} mb-3" role="status">
					<span>{albumNotice.message}</span>
					<button class="btn btn-sm" onclick={() => searchQuery.refetch()}>
						<RefreshCw class="h-4 w-4" /> Retry
					</button>
				</div>
			{/if}
			{#if loadingAlbums}
				<div class="bg-base-200 rounded-box p-4">
					<div
						class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
					>
						{#each Array(6) as _, i (`album-skeleton-${i}`)}
							<AlbumCardSkeleton />
						{/each}
					</div>
				</div>
			{:else if displayedAlbums.length > 0}
				<div class="bg-base-200 rounded-box p-4">
					<div
						class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
					>
						<ViewMoreAlbumCard />
						{#each displayedAlbums as album (album.musicbrainz_id)}
							<AlbumCard
								{album}
								{enrichmentSource}
								onadded={handleAlbumAdded}
								onenrichmentrequest={() => enrichCollector.requestAlbum(album)}
							/>
						{/each}
					</div>
				</div>
			{:else}
				<div class="p-8 bg-base-200 rounded-box text-center text-gray-500">No albums found</div>
			{/if}
		</div>
	</section>
{:else}
	<p class="text-center mt-32 text-gray-400">Enter a search query to get started.</p>
{/if}

{#if showToast}
	<div class="toast toast-end toast-bottom">
		<div class="alert alert-success">
			<Check class="h-6 w-6" />
			<span>Added to Library</span>
		</div>
	</div>
{/if}
