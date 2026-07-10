<script lang="ts">
	import { run } from 'svelte/legacy';

	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';
	import AlbumCard from '$lib/components/AlbumCard.svelte';
	import AlbumCardSkeleton from '$lib/components/AlbumCardSkeleton.svelte';
	import SearchTopResult from '$lib/components/SearchTopResult.svelte';
	import type { Album, EnrichmentSource } from '$lib/types';
	import SearchHeader from '$lib/components/search/SearchHeader.svelte';
	import { searchStore } from '$lib/stores/search';
	import { fetchEnrichmentBatch, applyAlbumEnrichment } from '$lib/utils/enrichment';
	import { isAbortError } from '$lib/utils/errorHandling';
	import { api } from '$lib/api/client';
	import { Check } from 'lucide-svelte';

	interface Props {
		data: { query: string };
	}

	let { data }: Props = $props();

	let albums: Album[] = $state([]);
	let topAlbum: Album | null = $state(null);
	let loading = $state(false);
	let hasMore = $state(true);
	let offset = 0;
	const limit = 24;
	let sentinel = $state<HTMLElement>();
	let showToast = $state(false);
	let abortController: AbortController | null = null;
	let enrichmentController: AbortController | null = null;
	let observer: IntersectionObserver | null = null;
	let enrichmentSource: EnrichmentSource = $state('none');
	let lastQuery = $state('');

	function handleAlbumAdded() {
		showToast = true;
		setTimeout(() => {
			showToast = false;
		}, 3000);
	}

	async function fetchEnrichment(albumsToEnrich: Album[]) {
		if (albumsToEnrich.length === 0) return;

		if (enrichmentController) {
			enrichmentController.abort();
		}
		enrichmentController = new AbortController();

		const requests = albumsToEnrich.map((a) => ({
			musicbrainz_id: a.musicbrainz_id,
			artist_name: a.artist || '',
			album_name: a.title
		}));

		try {
			const enrichment = await fetchEnrichmentBatch([], requests, enrichmentController.signal);
			if (!enrichment) return;

			enrichmentSource = enrichment.source;
			albums = applyAlbumEnrichment(albums, enrichment);
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
			const responseData = await api.get<{ results?: Album[]; top_result?: Album | null }>(
				`/api/v1/search/albums?q=${encodeURIComponent(data.query)}&limit=${limit}&offset=${offset}`,
				{ signal: abortController.signal }
			);

			const newAlbums: Album[] = responseData.results || [];
			if (offset === 0) {
				topAlbum = responseData.top_result ?? null;
			}
			if (newAlbums.length < limit) {
				hasMore = false;
			}

			// eslint-disable-next-line svelte/prefer-svelte-reactivity
			const newMbids = new Set<string>();
			if (offset === 0 && albums.length > 0) {
				const existingIds = new Set(albums.map((a) => a.musicbrainz_id));
				const uniqueNewAlbums = newAlbums.filter((a: Album) => !existingIds.has(a.musicbrainz_id));
				albums = [...albums, ...uniqueNewAlbums];
				offset = albums.length;
				uniqueNewAlbums.forEach((a) => newMbids.add(a.musicbrainz_id));
			} else {
				albums = [...albums, ...newAlbums];
				offset += newAlbums.length;
				newAlbums.forEach((a) => newMbids.add(a.musicbrainz_id));
			}
			searchStore.updateAlbums(albums);

			const toEnrich = albums.filter((a) => newMbids.has(a.musicbrainz_id));
			if (toEnrich.length > 0) {
				fetchEnrichment(toEnrich);
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
		if (cache && cache.albums.length > 0) {
			albums = cache.albums;
			topAlbum = cache.topAlbum ?? null;
			enrichmentSource = cache.enrichmentSource;
			offset = cache.albums.length;
			hasMore = cache.albums.length >= limit;
			const needsEnrichment = albums.filter((a) => a.listen_count == null);
			if (needsEnrichment.length > 0) {
				void fetchEnrichment(needsEnrichment);
			}

			if (searchStore.isStale(cache.timestamp)) {
				offset = 0;
				hasMore = true;
				void loadMore();
			}
		} else {
			albums = [];
			topAlbum = null;
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

<SearchHeader query={data.query} active="albums" />

<section class="px-8 py-4">
	{#if !data.query}
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
					<AlbumCard {album} {enrichmentSource} onadded={handleAlbumAdded} />
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
