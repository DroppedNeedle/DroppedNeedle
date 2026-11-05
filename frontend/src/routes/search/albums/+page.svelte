<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';
	import SearchAlbumCard from '$lib/features/search/components/SearchAlbumCard.svelte';
	import SearchEmpty from '$lib/features/search/components/SearchEmpty.svelte';
	import SearchHeader from '$lib/features/search/components/SearchHeader.svelte';
	import SearchNotice from '$lib/features/search/components/SearchNotice.svelte';
	import SearchShelf from '$lib/features/search/components/SearchShelf.svelte';
	import SearchTopResults from '$lib/features/search/components/SearchTopResults.svelte';
	import MediaTileSkeleton from '$lib/media/MediaTileSkeleton.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import type { Album, EnrichmentSource } from '$lib/features/album/queries/types';
	import type { SearchRemoteStatus } from '$lib/features/search/queries/types';
	import { applyAlbumEnrichment } from '$lib/features/search/queries/enrichment';
	import { getSearchStatusNotice } from '$lib/features/search/components/searchStatus';
	import {
		SEARCH_BUCKET_PAGE_SIZE,
		getSearchBucketV3Query,
		getSearchEnrichBatchV3Query,
		type EnrichmentResponseV3
	} from '$lib/features/search/queries/SearchV3Queries.svelte';
	import { SearchEnrichCollector } from '$lib/features/search/queries/SearchV3Enrichment.svelte';
	import { dedupeByMbid, toV1Album } from '$lib/features/search/queries/SearchV3Adapters';
	import { Disc3 } from 'lucide-svelte';
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
	let enrichmentSource: EnrichmentSource = $state(null);
	let sentinel = $state<HTMLElement>();

	const pageQuery = getSearchBucketV3Query(
		() => 'albums',
		() => normalizedQuery,
		() => SEARCH_BUCKET_PAGE_SIZE,
		() => offset
	);
	const enrichCollector = new SearchEnrichCollector();
	const enrichQuery = getSearchEnrichBatchV3Query(() => enrichCollector.body);

	// pages can repeat a release group, and the grid is keyed on its id
	let baseAlbums = $derived(
		dedupeByMbid(
			[...pages.entries()].sort(([left], [right]) => left - right).flatMap(([, page]) => page.items)
		)
	);
	let albums = $derived(enrichment ? applyAlbumEnrichment(baseAlbums, enrichment) : baseAlbums);
	let topAlbum = $derived(pages.get(0)?.top ?? null);
	let shownAlbums = $derived(topAlbum ? albums.filter((a) => !sameMbid(a, topAlbum)) : albums);

	// ids differ in case between MusicBrainz and library rows
	function sameMbid(album: Album, top: Album | null): boolean {
		return album.musicbrainz_id.toLowerCase() === top?.musicbrainz_id.toLowerCase();
	}
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

	function loadMore() {
		if (loading || !hasMore || !normalizedQuery) return;
		offset += SEARCH_BUCKET_PAGE_SIZE;
	}

	function resetAndLoad() {
		pages.clear();
		enrichCollector.reset();
		enrichment = null;
		enrichmentSource = null;
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

	$effect(() => {
		void normalizedQuery;
		offset = 0;
		pages.clear();
		enrichCollector.reset();
		enrichment = null;
		enrichmentSource = null;
	});

	$effect(() => {
		const page = pageQuery.data;
		if (!page || pages.get(page.offset)?.source === page) return;
		pages.set(page.offset, {
			items: page.results.map(toV1Album),
			top: page.top_result ? toV1Album(page.top_result) : null,
			status: page.status,
			source: page
		});
	});

	$effect(() => {
		const result = enrichQuery.data;
		if (result) {
			enrichment = result;
			enrichmentSource = result.source ?? null;
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

<SearchHeader query={normalizedQuery} current="albums" />

<div class="px-4 pt-4 pb-10 sm:px-6 lg:px-8">
	{#if normalizedQuery && statusNotice}
		<div class="mb-8">
			<SearchNotice notice={statusNotice} onretry={retryRemoteSearch} />
		</div>
	{/if}
	{#if !normalizedQuery}
		<p class="mt-12 text-center text-lg text-muted">Enter a search query to get started.</p>
	{:else if loading && albums.length === 0}
		<SearchShelf label="Loading albums" busy>
			{#each Array(14) as _, i (`loading-album-${i}`)}
				<MediaTileSkeleton />
			{/each}
		</SearchShelf>
	{:else if albums.length === 0 && !loading}
		<SearchEmpty icon={Disc3} title="No albums found" text="Try a different search." />
	{:else}
		{#if topAlbum}
			<section class="mb-10">
				<SectionHead title="Top album" />
				<SearchTopResults
					album={topAlbum}
					onalbumintent={() => topAlbum && enrichCollector.requestAlbum(topAlbum)}
				/>
			</section>
		{/if}
		<section>
			<SectionHead title="Albums" />
			<SearchShelf label="Album search results" busy={loading}>
				{#each shownAlbums as album, index (album.musicbrainz_id)}
					<SearchAlbumCard
						{album}
						lazy={index >= 14}
						{enrichmentSource}
						onenrichmentrequest={() => enrichCollector.requestAlbum(album)}
					/>
				{/each}
			</SearchShelf>
		</section>

		<div bind:this={sentinel} class="flex h-20 items-center justify-center">
			{#if loading}
				<span class="loading loading-spinner loading-md text-primary"></span>
			{:else if !hasMore}
				<p class="font-mono text-xs tracking-widest text-muted uppercase">No more results</p>
			{/if}
		</div>
	{/if}
</div>
