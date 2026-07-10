<script lang="ts">
	import { run } from 'svelte/legacy';

	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';
	import SearchArtistCard from '$lib/components/SearchArtistCard.svelte';
	import ArtistCardSkeleton from '$lib/components/ArtistCardSkeleton.svelte';
	import SearchTopResult from '$lib/components/SearchTopResult.svelte';
	import SearchHeader from '$lib/components/search/SearchHeader.svelte';
	import type { Artist, EnrichmentSource } from '$lib/types';
	import { searchStore } from '$lib/stores/search';
	import { fetchEnrichmentBatch, applyArtistEnrichment } from '$lib/utils/enrichment';
	import { isAbortError } from '$lib/utils/errorHandling';
	import { api } from '$lib/api/client';

	interface Props {
		data: { query: string };
	}

	let { data }: Props = $props();

	let artists: Artist[] = $state([]);
	let topArtist: Artist | null = $state(null);
	let loading = $state(false);
	let hasMore = $state(true);
	let offset = 0;
	const limit = 24;
	let sentinel = $state<HTMLElement>();
	let abortController: AbortController | null = null;
	let enrichmentController: AbortController | null = null;
	let observer: IntersectionObserver | null = null;
	let enrichmentSource: EnrichmentSource = $state('none');
	let lastQuery = $state('');

	async function fetchEnrichment(artistsToEnrich: Artist[]) {
		if (artistsToEnrich.length === 0) return;

		if (enrichmentController) {
			enrichmentController.abort();
		}
		enrichmentController = new AbortController();

		const requests = artistsToEnrich.map((a) => ({
			musicbrainz_id: a.musicbrainz_id,
			name: a.title
		}));

		try {
			const enrichment = await fetchEnrichmentBatch(requests, [], enrichmentController.signal);
			if (!enrichment) return;

			enrichmentSource = enrichment.source;
			artists = applyArtistEnrichment(artists, enrichment);
			searchStore.setEnrichmentSource(enrichmentSource);
		} catch (error) {
			if (isAbortError(error)) {
				return;
			}
		}
	}

	async function loadMore() {
		if (loading || !hasMore || !data.query) return;

		loading = true;

		if (abortController) {
			abortController.abort();
		}
		abortController = new AbortController();

		try {
			const responseData = await api.get<{ results?: Artist[]; top_result?: Artist | null }>(
				`/api/v1/search/artists?q=${encodeURIComponent(data.query)}&limit=${limit}&offset=${offset}`,
				{ signal: abortController.signal }
			);

			const newArtists: Artist[] = responseData.results || [];
			if (offset === 0) {
				topArtist = responseData.top_result ?? null;
			}
			if (newArtists.length < limit) {
				hasMore = false;
			}

			if (offset === 0 && artists.length > 0) {
				const existingIds = new Set(artists.map((a) => a.musicbrainz_id));
				const uniqueNewArtists = newArtists.filter(
					(a: Artist) => !existingIds.has(a.musicbrainz_id)
				);
				artists = [...artists, ...uniqueNewArtists];
				offset = artists.length;
			} else {
				artists = [...artists, ...newArtists];
				offset += newArtists.length;
			}
			searchStore.updateArtists(artists);

			const needsEnrichment = artists.filter((a) => a.release_group_count == null);
			if (needsEnrichment.length > 0) {
				fetchEnrichment(needsEnrichment);
			}
		} catch (error) {
			if (isAbortError(error)) {
				return;
			}
			hasMore = false;
		} finally {
			loading = false;
		}
	}

	function resetAndLoad() {
		if (abortController) {
			abortController.abort();
			abortController = null;
		}
		if (enrichmentController) {
			enrichmentController.abort();
			enrichmentController = null;
		}

		if (observer) {
			observer.disconnect();
			observer = null;
		}

		const cache = searchStore.getCache(data.query, { allowStale: true });
		if (cache && cache.artists.length > 0) {
			artists = cache.artists;
			topArtist = cache.topArtist ?? null;
			enrichmentSource = cache.enrichmentSource;
			offset = cache.artists.length;
			hasMore = cache.artists.length >= limit;
			const needsEnrichment = artists.filter((a) => a.release_group_count == null);
			if (needsEnrichment.length > 0) {
				void fetchEnrichment(needsEnrichment);
			}

			if (searchStore.isStale(cache.timestamp)) {
				offset = 0;
				hasMore = true;
				void loadMore();
			}
		} else {
			artists = [];
			topArtist = null;
			offset = 0;
			hasMore = true;
			enrichmentSource = 'none';
			void loadMore();
		}
	}

	run(() => {
		if (browser && data.query && data.query !== lastQuery) {
			lastQuery = data.query;
			resetAndLoad();
		}
	});

	run(() => {
		if (browser && sentinel && !observer) {
			observer = new IntersectionObserver(
				(entries) => {
					if (entries[0].isIntersecting && hasMore && !loading) {
						loadMore();
					}
				},
				{ threshold: 0.1 }
			);

			observer.observe(sentinel);
		}
	});

	onMount(() => {
		if (browser) {
			const handleRefresh = () => resetAndLoad();
			window.addEventListener('search-refresh', handleRefresh);

			return () => {
				window.removeEventListener('search-refresh', handleRefresh);
			};
		}
	});

	onDestroy(() => {
		if (observer) {
			observer.disconnect();
			observer = null;
		}
		if (abortController) {
			abortController.abort();
			abortController = null;
		}
		if (enrichmentController) {
			enrichmentController.abort();
			enrichmentController = null;
		}
	});
</script>

<SearchHeader query={data.query} active="artists" />

<section class="px-8 py-4">
	{#if !data.query}
		<p class="text-center mt-32 text-gray-400">Enter a search query to get started.</p>
	{:else if loading && artists.length === 0}
		<div class="bg-base-200 rounded-box p-4">
			<div
				class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
			>
				{#each Array(12) as _, i (`loading-artist-${i}`)}
					<ArtistCardSkeleton variant="detailed" />
				{/each}
			</div>
		</div>
	{:else if artists.length === 0 && !loading}
		<div class="p-8 bg-base-200 rounded-box text-center text-gray-500">No artists found</div>
	{:else}
		{#if topArtist}
			<div class="mb-4">
				<SearchTopResult artist={topArtist} />
			</div>
		{/if}
		<div class="bg-base-200 rounded-box p-4">
			<div
				class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
			>
				{#each topArtist ? artists.filter((a) => a.musicbrainz_id !== topArtist?.musicbrainz_id) : artists as artist (artist.musicbrainz_id)}
					<SearchArtistCard {artist} {enrichmentSource} />
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
