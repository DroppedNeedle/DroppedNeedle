<script lang="ts">
	import { getApiUrl } from '$lib/api/api-utils';
	import { page } from '$app/state';
	import { onMount, onDestroy } from 'svelte';
	import { beforeNavigate } from '$app/navigation';
	import GenreArtistCard from '$lib/components/GenreArtistCard.svelte';
	import GenreAlbumCard from '$lib/components/GenreAlbumCard.svelte';
	import { CACHE_KEYS, CACHE_TTL } from '$lib/constants';
	import type { GenreDetailResponse } from '$lib/types';
	import { createAbortable } from '$lib/utils/abortController';
	import { albumHrefOrNull, artistHrefOrNull } from '$lib/utils/entityRoutes';
	import { isAbortError } from '$lib/utils/errorHandling';
	import { api } from '$lib/api/client';
	import { createLocalStorageCache } from '$lib/utils/localStorageCache';
	import { ArrowLeft, BookOpen, Music2, CircleAlert, Mic, Disc3, ChevronDown } from 'lucide-svelte';
	import RadioPlayButton from '$lib/components/discover/RadioPlayButton.svelte';
	import type { RadioMode } from '$lib/player/launchRadio';
	import type { SampleEntry } from '$lib/stores/deckSampler.svelte';

	let genreName = $derived(page.url.searchParams.get('name') || '');
	let genreData: GenreDetailResponse | null = $state(null);
	let loading = $state(true);
	let error = $state('');
	let heroArtistMbid: string | null = $state(null);
	let heroImageLoaded = $state(false);
	const genreRequestAbortable = createAbortable();
	let lastLoadedGenre = '';

	let artistOffset = $state(0);
	let albumOffset = $state(0);
	let loadingMoreArtists = $state(false);
	let loadingMoreAlbums = $state(false);
	const PAGE_SIZE = 50;
	const genreDetailCache = createLocalStorageCache<GenreDetailResponse>(
		CACHE_KEYS.GENRE_DETAIL_CACHE,
		CACHE_TTL.GENRE_DETAIL,
		{ maxEntries: 60 }
	);

	function getGenreCacheSuffix(): string {
		return encodeURIComponent(genreName.trim().toLowerCase());
	}

	function persistGenreCache() {
		if (!genreData || !genreName) return;
		genreDetailCache.set(genreData, getGenreCacheSuffix());
	}

	async function loadHeroArtist() {
		if (!genreName) return;
		heroArtistMbid = null;
		heroImageLoaded = false;
		try {
			const data = await api.get<{ artist_mbid: string }>(
				`/api/v1/home/genre-artist/${encodeURIComponent(genreName)}`,
				{
					signal: genreRequestAbortable.signal
				}
			);
			heroArtistMbid = data.artist_mbid;
		} catch (e) {
			if (isAbortError(e)) return;
		}
	}

	async function loadGenreData() {
		if (!genreName) {
			error = 'No genre specified';
			loading = false;
			return;
		}

		const cacheSuffix = getGenreCacheSuffix();
		const cachedGenreData = genreDetailCache.get(cacheSuffix);
		const hasCachedGenreData = !!cachedGenreData?.data;
		const shouldRefresh = !cachedGenreData || genreDetailCache.isStale(cachedGenreData.timestamp);

		if (hasCachedGenreData) {
			genreData = cachedGenreData.data;
			loading = false;
		}

		if (!shouldRefresh) {
			error = '';
			return;
		}

		loading = !hasCachedGenreData;
		error = '';
		artistOffset = 0;
		albumOffset = 0;

		try {
			const data: GenreDetailResponse = await api.get(
				`/api/v1/home/genre/${encodeURIComponent(genreName)}?limit=${PAGE_SIZE}`,
				{ signal: genreRequestAbortable.signal }
			);
			genreData = data;
			genreDetailCache.set(data, cacheSuffix);
		} catch (e) {
			if (isAbortError(e)) return;
			if (!hasCachedGenreData) {
				error = "Couldn't load this genre";
			}
		} finally {
			if (!hasCachedGenreData) {
				loading = false;
			}
		}
	}

	async function loadMoreArtists() {
		if (!genreData || loadingMoreArtists || !genreData.popular?.has_more_artists) return;
		loadingMoreArtists = true;
		artistOffset += PAGE_SIZE;

		try {
			const data: GenreDetailResponse = await api.get(
				`/api/v1/home/genre/${encodeURIComponent(genreName)}?limit=${PAGE_SIZE}&artist_offset=${artistOffset}`,
				{ signal: genreRequestAbortable.signal }
			);
			if (genreData.popular && data.popular) {
				genreData.popular.artists = [...genreData.popular.artists, ...data.popular.artists];
				genreData.popular.has_more_artists = data.popular.has_more_artists;
				persistGenreCache();
			}
		} catch (e) {
			if (isAbortError(e)) return;
			artistOffset -= PAGE_SIZE;
		} finally {
			loadingMoreArtists = false;
		}
	}

	async function loadMoreAlbums() {
		if (!genreData || loadingMoreAlbums || !genreData.popular?.has_more_albums) return;
		loadingMoreAlbums = true;
		albumOffset += PAGE_SIZE;

		try {
			const data: GenreDetailResponse = await api.get(
				`/api/v1/home/genre/${encodeURIComponent(genreName)}?limit=${PAGE_SIZE}&album_offset=${albumOffset}`,
				{ signal: genreRequestAbortable.signal }
			);
			if (genreData.popular && data.popular) {
				genreData.popular.albums = [...genreData.popular.albums, ...data.popular.albums];
				genreData.popular.has_more_albums = data.popular.has_more_albums;
				persistGenreCache();
			}
		} catch (e) {
			if (isAbortError(e)) return;
			albumOffset -= PAGE_SIZE;
		} finally {
			loadingMoreAlbums = false;
		}
	}

	function loadData() {
		genreRequestAbortable.reset();
		lastLoadedGenre = genreName;
		void loadGenreData();
		void loadHeroArtist();
	}

	function cleanup() {
		genreRequestAbortable.abort();
	}

	onMount(() => {
		if (genreName) loadData();
	});
	onDestroy(cleanup);
	beforeNavigate(cleanup);

	$effect(() => {
		if (genreName && genreName !== lastLoadedGenre) loadData();
	});

	const hasLibraryContent = $derived.by(() => {
		const data = genreData;
		return (data?.library?.artists?.length ?? 0) > 0 || (data?.library?.albums?.length ?? 0) > 0;
	});

	// station mode: full tracks (library + YouTube), 30s previews, or library-only
	let radioMode = $state<RadioMode | 'previews'>('hybrid');
	const effectiveMode = $derived<RadioMode>(radioMode === 'previews' ? 'hybrid' : radioMode);

	// Quick-previews station: hear this genre's albums as 30s clips in the widget.
	// Popular (discovery) albums first, then library, deduped.
	const previewStation = $derived.by(() => {
		const seen: Record<string, true> = {};
		const entries: SampleEntry[] = [];
		const albums = [...(genreData?.popular?.albums ?? []), ...(genreData?.library?.albums ?? [])];
		for (const album of albums) {
			if (!album.mbid || seen[album.mbid]) continue;
			seen[album.mbid] = true;
			entries.push({
				key: album.mbid,
				kind: 'album',
				artist: album.artist_name ?? '',
				title: album.name,
				albumMbid: album.mbid,
				artistMbid: album.artist_mbid,
				coverUrl: album.image_url
			});
		}
		return { title: genreName ? `${genreName} previews` : 'Genre previews', entries };
	});
</script>

<svelte:head>
	<title>{genreName ? `${genreName}` : 'Genre'} - DroppedNeedle</title>
</svelte:head>

<div class="relative min-h-screen overflow-hidden bg-base-100">
	{#if heroArtistMbid}
		<!-- cinematic backdrop: genre artist portrait under a charcoal wash -->
		<div
			class="pointer-events-none absolute inset-x-0 top-0 h-[26rem] overflow-hidden"
			style="z-index: 0;"
		>
			<img
				src={getApiUrl(`/api/v1/covers/artist/${heroArtistMbid}?size=500`)}
				alt=""
				class="h-full w-full object-cover object-top transition-opacity duration-700 {heroImageLoaded
					? 'opacity-100'
					: 'opacity-0'}"
				style="filter: saturate(0.9) brightness(0.72);"
				onload={() => (heroImageLoaded = true)}
			/>
			<div
				class="absolute inset-0 bg-linear-to-r from-base-100/90 via-base-100/55 to-base-100/25"
			></div>
			<div
				class="absolute inset-0 bg-linear-to-b from-base-100/35 via-base-100/60 to-base-100"
			></div>
		</div>
	{/if}

	<div class="container relative mx-auto max-w-7xl p-4" style="z-index: 1;">
		<header class="mb-10 pt-4">
			<a href="/" class="btn btn-ghost btn-sm mb-6 gap-2 rounded-full bg-base-content/6">
				<ArrowLeft class="h-4 w-4" />
				Back
			</a>
			<p
				class="mb-3 w-fit rounded-full border border-base-content/15 bg-base-100/45 px-3 py-1 font-mono text-[0.62rem] font-bold uppercase tracking-[0.22em] text-base-content/70 backdrop-blur-sm"
			>
				Genre
			</p>
			<div class="flex items-center gap-5">
				<div
					class="flex h-20 w-20 shrink-0 items-center justify-center rounded-2xl border border-base-content/8 bg-base-200/60 backdrop-blur-sm"
				>
					<Music2 class="h-10 w-10 text-accent" />
				</div>
				<div>
					<h1
						class="hero-title font-display text-4xl font-bold capitalize tracking-tight sm:text-6xl"
					>
						{genreName || 'Genre'}
					</h1>
					{#if genreData}
						<p class="mt-2 text-sm text-base-content/55 sm:text-base">
							{#if hasLibraryContent}
								{genreData.library?.artist_count ?? 0} artists · {genreData.library?.album_count ??
									0} albums in your library
							{:else}
								Explore popular {genreName} music
							{/if}
						</p>
					{/if}
				</div>
			</div>
			{#if genreName}
				<div class="mt-6 flex flex-wrap items-center gap-3">
					<RadioPlayButton
						seed={{ seed_type: 'genre', seed_id: genreName, mode: effectiveMode }}
						mode={effectiveMode}
						forcePreviews={radioMode === 'previews'}
						previewStation={radioMode === 'previews' ? previewStation : null}
						showShuffle={true}
						size="md"
						label="Play"
					/>
					<div
						class="flex items-center gap-1 rounded-full border border-base-content/8 bg-base-200/60 p-1 backdrop-blur-sm"
						role="radiogroup"
						aria-label="Station mode"
					>
						<button
							class="btn btn-xs rounded-full {radioMode === 'hybrid' ? 'btn-primary' : 'btn-ghost'}"
							onclick={() => (radioMode = 'hybrid')}
							title="Your library plus streamed full tracks"
						>
							Full tracks
						</button>
						<button
							class="btn btn-xs rounded-full {radioMode === 'previews'
								? 'btn-primary'
								: 'btn-ghost'}"
							onclick={() => (radioMode = 'previews')}
							title="30-second tastes - fast crate-digging"
						>
							Quick previews
						</button>
						<button
							class="btn btn-xs rounded-full {radioMode === 'library'
								? 'btn-primary'
								: 'btn-ghost'}"
							onclick={() => (radioMode = 'library')}
							title="Only music you own"
						>
							My library
						</button>
					</div>
				</div>
			{/if}
		</header>

		{#if loading}
			<section class="mb-12" aria-label="Loading">
				<div class="mb-6 flex items-center gap-3">
					<div class="skeleton h-4 w-4 rounded"></div>
					<div class="skeleton h-4 w-44"></div>
				</div>
				<div
					class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
				>
					{#each Array(12) as _, i (`genre-artist-skeleton-${i}`)}
						<div class="rounded-2xl border border-base-content/8 bg-base-200/50">
							<div class="skeleton aspect-square rounded-t-2xl"></div>
							<div class="p-3">
								<div class="skeleton mb-2 h-4 w-3/4"></div>
								<div class="skeleton h-3 w-1/2"></div>
							</div>
						</div>
					{/each}
				</div>
			</section>

			<section class="mb-12" aria-label="Loading">
				<div class="mb-6 flex items-center gap-3">
					<div class="skeleton h-4 w-4 rounded"></div>
					<div class="skeleton h-4 w-36"></div>
				</div>
				<div
					class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
				>
					{#each Array(6) as _, i (`genre-album-skeleton-${i}`)}
						<div class="rounded-2xl border border-base-content/8 bg-base-200/50">
							<div class="skeleton aspect-square rounded-t-2xl"></div>
							<div class="p-3">
								<div class="skeleton mb-2 h-4 w-3/4"></div>
								<div class="skeleton h-3 w-1/2"></div>
							</div>
						</div>
					{/each}
				</div>
			</section>
		{:else if error}
			<div class="flex flex-col items-center justify-center py-24">
				<CircleAlert class="mb-4 h-12 w-12 text-base-content/40" strokeWidth={1.5} />
				<p class="text-lg text-base-content/70">{error}</p>
				<button class="btn btn-primary mt-6 rounded-full" onclick={loadData}>Try Again</button>
			</div>
		{:else if genreData}
			{#if hasLibraryContent}
				<section class="mb-12" aria-label="From Your Library">
					<div class="mb-5 flex items-baseline justify-between gap-3">
						<h2
							class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
						>
							<BookOpen class="h-4 w-4 text-accent" />
							From your library
						</h2>
						<p class="text-xs text-base-content/45">
							{genreData.library?.artist_count ?? 0} artists · {genreData.library?.album_count ?? 0}
							albums
						</p>
					</div>

					{#if (genreData.library?.artists?.length ?? 0) > 0}
						<h3 class="mb-4 font-display text-lg font-semibold tracking-tight text-base-content/80">
							Artists
						</h3>
						<div
							class="mb-8 grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
						>
							{#each genreData.library?.artists ?? [] as artist (artist.mbid || artist.name)}
								<GenreArtistCard
									{artist}
									showLibraryBadge={true}
									href={artistHrefOrNull(artist.mbid)}
								/>
							{/each}
						</div>
					{/if}

					{#if (genreData.library?.albums?.length ?? 0) > 0}
						<h3 class="mb-4 font-display text-lg font-semibold tracking-tight text-base-content/80">
							Albums
						</h3>
						<div
							class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
						>
							{#each genreData.library?.albums ?? [] as album (album.mbid || album.name)}
								<GenreAlbumCard
									{album}
									showLibraryBadge={true}
									href={albumHrefOrNull(album.mbid)}
								/>
							{/each}
						</div>
					{/if}
				</section>
				<div class="my-10 h-px bg-base-content/8" aria-hidden="true"></div>
			{/if}

			<section class="mb-12" aria-label="Popular Artists">
				<div class="mb-5 flex items-baseline justify-between gap-3">
					<h2
						class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
					>
						<Mic class="h-4 w-4 text-accent" />
						Popular artists
					</h2>
					<p class="text-xs text-base-content/45">Top {genreName} artists</p>
				</div>

				{#if (genreData.popular?.artists?.length ?? 0) === 0}
					<div
						class="flex flex-col items-center justify-center rounded-2xl border border-dashed border-base-content/12 py-16"
					>
						<Mic class="mb-4 h-10 w-10 text-base-content/30" strokeWidth={1.5} />
						<p class="text-base-content/50">No artists found for this genre</p>
					</div>
				{:else}
					<div
						class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
					>
						{#each genreData.popular?.artists ?? [] as artist (artist.mbid || artist.name)}
							<GenreArtistCard {artist} href={artistHrefOrNull(artist.mbid)} />
						{/each}
					</div>
					{#if genreData.popular?.has_more_artists}
						<div class="mt-8 flex justify-center">
							<button
								class="btn btn-wide gap-2 rounded-full btn-ghost bg-base-content/6"
								onclick={loadMoreArtists}
								disabled={loadingMoreArtists}
							>
								{#if loadingMoreArtists}
									<span class="loading loading-spinner loading-sm"></span>
								{:else}
									<ChevronDown class="h-4 w-4" />
								{/if}
								View More Artists
							</button>
						</div>
					{/if}
				{/if}
			</section>

			<section class="mb-12" aria-label="Popular Albums">
				<div class="mb-5 flex items-baseline justify-between gap-3">
					<h2
						class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
					>
						<Disc3 class="h-4 w-4 text-accent" />
						Popular albums
					</h2>
					<p class="text-xs text-base-content/45">Top {genreName} albums</p>
				</div>

				{#if (genreData.popular?.albums?.length ?? 0) === 0}
					<div
						class="flex flex-col items-center justify-center rounded-2xl border border-dashed border-base-content/12 py-16"
					>
						<Disc3 class="mb-4 h-10 w-10 text-base-content/30" strokeWidth={1.5} />
						<p class="text-base-content/50">No albums found for this genre</p>
					</div>
				{:else}
					<div
						class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
					>
						{#each genreData.popular?.albums ?? [] as album (album.mbid || album.name)}
							<GenreAlbumCard {album} href={albumHrefOrNull(album.mbid)} />
						{/each}
					</div>
					{#if genreData.popular?.has_more_albums}
						<div class="mt-8 flex justify-center">
							<button
								class="btn btn-wide gap-2 rounded-full btn-ghost bg-base-content/6"
								onclick={loadMoreAlbums}
								disabled={loadingMoreAlbums}
							>
								{#if loadingMoreAlbums}
									<span class="loading loading-spinner loading-sm"></span>
								{:else}
									<ChevronDown class="h-4 w-4" />
								{/if}
								View More Albums
							</button>
						</div>
					{/if}
				{/if}
			</section>
		{/if}
	</div>
</div>
