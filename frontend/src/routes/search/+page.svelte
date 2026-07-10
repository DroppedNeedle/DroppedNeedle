<script lang="ts">
	import { run } from 'svelte/legacy';

	import { onMount, onDestroy } from 'svelte';
	import { browser } from '$app/environment';
	import AlbumCard from '$lib/components/AlbumCard.svelte';
	import SearchArtistCard from '$lib/components/SearchArtistCard.svelte';
	import ViewMoreAlbumCard from '$lib/components/ViewMoreAlbumCard.svelte';
	import ViewMoreArtistCard from '$lib/components/ViewMoreArtistCard.svelte';
	import ArtistCardSkeleton from '$lib/components/ArtistCardSkeleton.svelte';
	import AlbumCardSkeleton from '$lib/components/AlbumCardSkeleton.svelte';
	import type { Artist, Album, EnrichmentSource } from '$lib/types';
	import { searchStore } from '$lib/stores/search';
	import {
		fetchEnrichmentBatch,
		applyArtistEnrichment,
		applyAlbumEnrichment
	} from '$lib/utils/enrichment';
	import { isAbortError } from '$lib/utils/errorHandling';
	import { api } from '$lib/api/client';
	import { Check, ArrowRight } from 'lucide-svelte';
	import SearchTopResult from '$lib/components/SearchTopResult.svelte';
	import SearchHeader from '$lib/components/search/SearchHeader.svelte';

	interface Props {
		data: { query: string };
	}

	let { data }: Props = $props();

	let artists: Artist[] = $state([]);
	let albums: Album[] = $state([]);
	let topArtist: Artist | null = $state(null);
	let topAlbum: Album | null = $state(null);
	let loadingArtists = $state(false);
	let loadingAlbums = $state(false);
	let hasSearched = $state(false);
	let showToast = $state(false);
	let abortController: AbortController | null = null;
	let enrichmentController: AbortController | null = null;
	let enrichmentSource: EnrichmentSource = $state('none');

	let isSearching = $derived(loadingArtists || loadingAlbums);
	let hasResults = $derived(artists.length > 0 || albums.length > 0);
	let hasTopResult = $derived(topArtist != null || topAlbum != null);
	let displayedArtists = $derived(
		topArtist ? artists.filter((a) => a.musicbrainz_id !== topArtist?.musicbrainz_id) : artists
	);
	let displayedAlbums = $derived(
		topAlbum ? albums.filter((a) => a.musicbrainz_id !== topAlbum?.musicbrainz_id) : albums
	);

	function handleAlbumAdded() {
		showToast = true;
		setTimeout(() => {
			showToast = false;
		}, 3000);
	}

	async function fetchEnrichment() {
		if (artists.length === 0 && albums.length === 0) return;

		if (enrichmentController) {
			enrichmentController.abort();
		}
		enrichmentController = new AbortController();

		const artistRequests = artists.map((a) => ({
			musicbrainz_id: a.musicbrainz_id,
			name: a.title
		}));
		const albumRequests = albums.map((a) => ({
			musicbrainz_id: a.musicbrainz_id,
			artist_name: a.artist || '',
			album_name: a.title
		}));

		try {
			const enrichment = await fetchEnrichmentBatch(
				artistRequests,
				albumRequests,
				enrichmentController.signal
			);
			if (!enrichment) return;

			enrichmentSource = enrichment.source;
			artists = applyArtistEnrichment(artists, enrichment);
			albums = applyAlbumEnrichment(albums, enrichment);
			searchStore.setEnrichmentSource(enrichmentSource);
		} catch (error) {
			if (isAbortError(error)) {
				return;
			}
		}
	}

	async function performSearch(q: string) {
		const normalizedQuery = q.trim();
		const cached = searchStore.getCache(normalizedQuery, { allowStale: true });
		const hasCachedResults = !!cached;
		const shouldRefresh = !cached || searchStore.isStale(cached.timestamp);

		if (cached) {
			hasSearched = true;
			artists = cached.artists;
			albums = cached.albums;
			topArtist = cached.topArtist ?? null;
			topAlbum = cached.topAlbum ?? null;
			enrichmentSource = cached.enrichmentSource;
			loadingArtists = false;
			loadingAlbums = false;
		}

		if (abortController) {
			abortController.abort();
		}
		if (enrichmentController) {
			enrichmentController.abort();
		}

		if (!normalizedQuery) {
			artists = [];
			albums = [];
			topArtist = null;
			topAlbum = null;
			hasSearched = false;
			enrichmentSource = 'none';
			searchStore.clear();
			return;
		}

		if (!shouldRefresh) {
			return;
		}

		abortController = new AbortController();
		hasSearched = true;
		if (!hasCachedResults) {
			artists = [];
			albums = [];
			enrichmentSource = 'none';
		}
		loadingArtists = true;
		loadingAlbums = true;

		try {
			const responseData = await api.get<{
				artists?: Artist[];
				albums?: Album[];
				top_artist?: Artist | null;
				top_album?: Album | null;
			}>(
				`/api/v1/search?q=${encodeURIComponent(normalizedQuery)}&limit_artists=6&limit_albums=24`,
				{ signal: abortController.signal }
			);
			artists = responseData.artists || [];
			albums = responseData.albums || [];
			topArtist = responseData.top_artist ?? null;
			topAlbum = responseData.top_album ?? null;
		} catch (e) {
			if (isAbortError(e)) {
				return;
			}
			artists = [];
			albums = [];
			topArtist = null;
			topAlbum = null;
		} finally {
			loadingArtists = false;
			loadingAlbums = false;
		}

		searchStore.setResults(normalizedQuery, artists, albums, enrichmentSource, topArtist, topAlbum);

		void fetchEnrichment();
	}

	let lastQuery = $state('');

	run(() => {
		if (browser && data.query && data.query !== lastQuery) {
			lastQuery = data.query;
			performSearch(data.query);
		} else if (browser && !data.query) {
			artists = [];
			albums = [];
			topArtist = null;
			topAlbum = null;
			hasSearched = false;
			lastQuery = '';
			searchStore.clear();
		}
	});

	onMount(() => {
		if (browser) {
			const handleRefresh = () => {
				if (data.query) {
					performSearch(data.query);
				}
			};
			window.addEventListener('search-refresh', handleRefresh);

			return () => {
				window.removeEventListener('search-refresh', handleRefresh);
			};
		}
	});

	onDestroy(() => {
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

{#if hasSearched || isSearching}
	<SearchHeader query={data.query} active="all" />
{/if}

{#if isSearching && !hasResults}
	<section class="px-8 py-4 space-y-8">
		<div>
			<h2 class="text-xl font-bold mb-4">Artists</h2>
			<div class="bg-base-200 rounded-box p-4">
				<div
					class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
				>
					{#each Array(6) as _, i (`artist-skeleton-${i}`)}
						<ArtistCardSkeleton variant="detailed" />
					{/each}
				</div>
			</div>
		</div>

		<div>
			<div class="flex items-center justify-between mb-4">
				<h2 class="text-xl font-bold">Albums</h2>
			</div>
			<div class="bg-base-200 rounded-box p-4">
				<div
					class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
				>
					{#each Array(6) as _, i (`album-skeleton-${i}`)}
						<AlbumCardSkeleton />
					{/each}
				</div>
			</div>
		</div>
	</section>
{:else if hasSearched}
	<section class="px-8 py-4 space-y-8">
		{#if hasTopResult && !isSearching}
			<div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
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
					>
						<ViewMoreArtistCard />
						{#each displayedArtists.slice(0, 5) as artist (artist.musicbrainz_id)}
							<SearchArtistCard {artist} {enrichmentSource} />
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
						href={`/search/albums?q=${encodeURIComponent(data.query)}`}
						class="text-sm text-primary hover:underline"
					>
						View more <ArrowRight class="h-4 w-4 inline align-middle" />
					</a>
				{/if}
			</div>
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
							<AlbumCard {album} {enrichmentSource} onadded={handleAlbumAdded} />
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
