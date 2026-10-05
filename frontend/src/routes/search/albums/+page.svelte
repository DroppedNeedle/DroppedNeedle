<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import AlbumCard from '$lib/components/AlbumCard.svelte';
	import AlbumCardSkeleton from '$lib/components/AlbumCardSkeleton.svelte';
	import SearchTopResult from '$lib/components/SearchTopResult.svelte';
	import type { Album, EnrichmentSource, SearchRemoteStatus } from '$lib/types';
	import { colors } from '$lib/colors';
	import { applyAlbumEnrichment } from '$lib/utils/enrichment';
	import { getSearchStatusNotice } from '$lib/utils/searchStatus';
	import {
		SEARCH_BUCKET_PAGE_SIZE,
		getSearchBucketV3Query,
		getSearchEnrichBatchV3Query,
		type EnrichmentResponseV3
	} from '$lib/queries/search/SearchV3Queries.svelte';
	import { SearchEnrichCollector } from '$lib/queries/search/SearchV3Enrichment.svelte';
	import { toSearchRemoteStatus, toV1Album } from '$lib/queries/search/SearchV3Adapters';
	import { Check, RefreshCw } from 'lucide-svelte';
	import { SvelteMap } from 'svelte/reactivity';

	interface Props {
		data: { query: string };
	}

	let { data }: Props = $props();

	let normalizedQuery = $derived(data.query.trim());
	let offset = $state(0);
	// Answered pages filed under their echoed offset, so pages land in
	// order even if they resolve out of order. The stored source is the raw
	// answered page itself: identity comparison tells a fresh answer from
	// an already-filed one, so a retry overwrite never gets mistaken for
	// a duplicate filing.
	let pages = new SvelteMap<
		number,
		{ items: Album[]; top: Album | null; status: SearchRemoteStatus; source: unknown }
	>();
	let enrichment: EnrichmentResponseV3 | null = $state(null);
	let enrichmentSource: EnrichmentSource = $state('none');
	let sentinel = $state<HTMLElement>();
	let showToast = $state(false);

	const pageQuery = getSearchBucketV3Query(
		() => 'albums',
		() => normalizedQuery,
		() => SEARCH_BUCKET_PAGE_SIZE,
		() => offset
	);
	const enrichCollector = new SearchEnrichCollector();
	const enrichQuery = getSearchEnrichBatchV3Query(() => enrichCollector.body);

	let baseAlbums = $derived(
		[...pages.entries()].sort(([left], [right]) => left - right).flatMap(([, page]) => page.items)
	);
	let albums = $derived(enrichment ? applyAlbumEnrichment(baseAlbums, enrichment) : baseAlbums);
	let topAlbum = $derived(pages.get(0)?.top ?? null);
	let remoteStatus: SearchRemoteStatus = $derived(
		pageQuery.isError ? 'error' : (pages.get(0)?.status ?? 'ok')
	);
	let statusNotice = $derived(getSearchStatusNotice(remoteStatus, 'albums', false));
	let loading = $derived(pageQuery.isPending || pageQuery.isFetching);
	let hasMore = $derived.by(() => {
		if (pages.size === 0) return true;
		const lastOffset = Math.max(...pages.keys());
		return (pages.get(lastOffset)?.items.length ?? 0) >= SEARCH_BUCKET_PAGE_SIZE;
	});

	function navigateBack() {
		if (normalizedQuery) {
			goto(withBasePath(`/search?q=${encodeURIComponent(normalizedQuery)}`));
		}
	}

	function navigateToBucket(bucket: 'artists') {
		if (normalizedQuery) {
			goto(withBasePath(`/search/${bucket}?q=${encodeURIComponent(normalizedQuery)}`));
		}
	}

	function loadMore() {
		if (loading || !hasMore || !normalizedQuery) return;
		offset += SEARCH_BUCKET_PAGE_SIZE;
	}

	function resetAndLoad() {
		pages.clear();
		enrichCollector.reset();
		enrichment = null;
		enrichmentSource = 'none';
		if (offset === 0) {
			void pageQuery.refetch();
		} else {
			offset = 0;
		}
	}

	function retryRemoteSearch() {
		if (loading || !normalizedQuery) return;
		resetAndLoad();
	}

	function handleAlbumAdded() {
		showToast = true;
		setTimeout(() => {
			showToast = false;
		}, 3000);
	}

	$effect(() => {
		void normalizedQuery;
		offset = 0;
		pages.clear();
		enrichCollector.reset();
		enrichment = null;
		enrichmentSource = 'none';
	});

	$effect(() => {
		const page = pageQuery.data;
		if (!page || pages.get(page.offset)?.source === page) return;
		pages.set(page.offset, {
			items: page.results.map(toV1Album),
			top: page.top_result ? toV1Album(page.top_result) : null,
			status: toSearchRemoteStatus(page.status),
			source: page
		});
	});

	$effect(() => {
		const result = enrichQuery.data;
		if (result) {
			enrichment = result;
			enrichmentSource = result.source;
		}
	});

	$effect(() => {
		if (!browser || !sentinel) return;
		const current = new IntersectionObserver(
			(entries) => {
				if (entries[0].isIntersecting) loadMore();
			},
			{ threshold: 0.1 }
		);
		current.observe(sentinel);
		return () => current.disconnect();
	});

	onMount(() => {
		if (!browser) return;
		const handleRefresh = () => resetAndLoad();
		window.addEventListener('search-refresh', handleRefresh);
		return () => window.removeEventListener('search-refresh', handleRefresh);
	});

	onDestroy(() => {
		enrichCollector.dispose();
	});
</script>

<div class="px-8 pt-4 pb-2">
	<div class="flex gap-2">
		<button
			class="badge badge-lg cursor-pointer transition-colors"
			style="background-color: {colors.secondary}; color: {colors.primary};"
			onclick={navigateBack}
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
			class="badge badge-lg cursor-pointer"
			style="background-color: {colors.primary}; color: {colors.secondary};"
		>
			Albums
		</button>
	</div>
</div>
<section class="px-8 py-4">
	{#if normalizedQuery && statusNotice}
		<div class="alert {statusNotice.className} mb-3" role="status">
			<span>{statusNotice.message}</span>
			<button class="btn btn-sm" onclick={retryRemoteSearch}>
				<RefreshCw class="h-4 w-4" /> Retry
			</button>
		</div>
	{/if}
	{#if !normalizedQuery}
		<p class="text-center mt-32 text-gray-400">Enter a search query to get started.</p>
	{:else if loading && albums.length === 0}
		<div class="bg-base-200 rounded-box p-4">
			<div
				class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
			>
				{#each Array(12) as _, i (`loading-album-${i}`)}
					<AlbumCardSkeleton />
				{/each}
			</div>
		</div>
	{:else if albums.length === 0 && !loading}
		<div class="p-8 bg-base-200 rounded-box text-center text-gray-500">No albums found</div>
	{:else}
		{#if topAlbum}
			<div class="mb-4">
				<SearchTopResult album={topAlbum} />
			</div>
		{/if}
		<div class="bg-base-200 rounded-box p-4">
			<div
				class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
			>
				{#each topAlbum ? albums.filter((a) => a.musicbrainz_id !== topAlbum?.musicbrainz_id) : albums as album (album.musicbrainz_id)}
					<AlbumCard
						{album}
						{enrichmentSource}
						onadded={handleAlbumAdded}
						onenrichmentrequest={() => enrichCollector.requestAlbum(album)}
					/>
				{/each}
			</div>
		</div>

		<div bind:this={sentinel} class="h-20 flex items-center justify-center">
			{#if loading}
				<span class="loading loading-spinner loading-md text-primary"></span>
			{:else if !hasMore}
				<p class="text-gray-400 text-sm">No more results</p>
			{/if}
		</div>
	{/if}
</section>

{#if showToast}
	<div class="toast toast-end toast-bottom">
		<div class="alert alert-success">
			<Check class="h-6 w-6" />
			<span>Added to Library</span>
		</div>
	</div>
{/if}
