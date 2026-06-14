<script lang="ts">
	import { onMount } from 'svelte';
	import { fade } from 'svelte/transition';
	import ArtistCard from '$lib/components/ArtistCard.svelte';
	import AlbumCard from '$lib/components/AlbumCard.svelte';
	import ArtistCardSkeleton from '$lib/components/ArtistCardSkeleton.svelte';
	import AlbumCardSkeleton from '$lib/components/AlbumCardSkeleton.svelte';
	import HorizontalCarousel from '$lib/components/HorizontalCarousel.svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import { recentlyAddedStore } from '$lib/stores/recentlyAdded';
	import { syncStatus } from '$lib/stores/syncStatus.svelte';
	import { api, ApiError } from '$lib/api/client';
	import { API } from '$lib/constants';
	import { isAbortError } from '$lib/utils/errorHandling';
	import type { Artist, Album } from '$lib/types';
	import {
		CircleX,
		X,
		RefreshCw,
		ChevronRight,
		Search,
		Loader2,
		Settings2,
		Eye
	} from 'lucide-svelte';

	const CIRCUIT_BREAKER_CODE = 'CIRCUIT_BREAKER_OPEN';

	type LibraryArtist = {
		name: string;
		mbid: string;
		album_count: number;
		date_added: string | null;
	};

	type LibraryAlbum = {
		album: string;
		artist: string;
		artist_mbid: string | null;
		foreignAlbumId: string | null;
		year: number | null;
		monitored: boolean;
		cover_url: string | null;
		date_added: number | null;
	};

	type PaginatedAlbumsResponse = {
		albums: LibraryAlbum[];
		total: number;
		offset: number;
		limit: number;
	};

	type PaginatedArtistsResponse = {
		artists: LibraryArtist[];
		total: number;
		offset: number;
		limit: number;
	};

	type LibraryStats = {
		artist_count: number;
		album_count: number;
		last_sync: number | null;
		db_size_mb: number;
	};

	const ALBUMS_PER_PAGE = 50;
	const ARTIST_CAROUSEL_LIMIT = 50;
	const SEARCH_DEBOUNCE_MS = 300;

	let albums: LibraryAlbum[] = $state([]);
	let albumsTotal = $state(0);
	let artists: LibraryArtist[] = $state([]);
	let stats: LibraryStats = $state({
		artist_count: 0,
		album_count: 0,
		last_sync: null,
		db_size_mb: 0
	});

	let loadingArtists = $state(true);
	let loadingAlbums = $state(true);
	let loadingStats = $state(true);
	let syncing = $state(false);
	let error: string | null = $state(null);
	let errorCode: string | null = $state(null);
	let syncFrequencyLabel: string | null = $state(null);

	let currentAlbumPage = $state(1);
	let sortBy = $state('date_added');
	let sortOrder = $state('desc');
	let searchQuery = $state('');
	let searchTimeout: ReturnType<typeof setTimeout> | null = null;
	let albumsFetchId = 0;
	let albumsAbort: AbortController | null = null;

	let recentlyAdded = $derived($recentlyAddedStore.data ?? { artists: [], albums: [] });
	let loadingRecentlyAdded = $derived($recentlyAddedStore.loading && !$recentlyAddedStore.data);
	let isSearching = $derived(searchQuery.trim().length > 0);
	let totalAlbumPages = $derived(Math.ceil(albumsTotal / ALBUMS_PER_PAGE));
	let lastSyncText = $derived(
		stats.last_sync ? new Date(stats.last_sync * 1000).toLocaleString() : 'Never'
	);
	let isConnectionError = $derived(
		errorCode === CIRCUIT_BREAKER_CODE ||
			(error != null && /connection|DNS|not configured/i.test(error))
	);

	const FREQ_LABELS: Record<string, string> = {
		manual: 'Manual sync only',
		'5min': 'Auto-syncs every 5 minutes',
		'10min': 'Auto-syncs every 10 minutes',
		'30min': 'Auto-syncs every 30 minutes',
		'1hr': 'Auto-syncs every hour',
		'6hr': 'Auto-syncs every 6 hours',
		'12hr': 'Auto-syncs every 12 hours',
		'24hr': 'Auto-syncs every 24 hours',
		'3d': 'Auto-syncs every 3 days',
		'7d': 'Auto-syncs every 7 days'
	};

	onMount(() => {
		recentlyAddedStore.initialize();
		loadArtists();
		fetchAlbums();
		loadStats();
		loadSyncFrequency();
	});

	async function loadSyncFrequency() {
		try {
			const data = await api.global.get<{ sync_frequency: string }>('/api/v1/settings/lidarr');
			syncFrequencyLabel = FREQ_LABELS[data.sync_frequency] ?? null;
		} catch {
			// non-critical; use default label
		}
	}

	async function loadArtists() {
		try {
			const url = API.library.artists(ARTIST_CAROUSEL_LIMIT, 0, 'name', 'asc');
			const data = await api.get<PaginatedArtistsResponse>(url);
			artists = data.artists;
		} catch (e) {
			if (isAbortError(e)) return;
			console.error("Couldn't load artists:", e);
		} finally {
			loadingArtists = false;
		}
	}

	async function fetchAlbums() {
		const id = ++albumsFetchId;
		albumsAbort?.abort();
		albumsAbort = new AbortController();
		loadingAlbums = true;
		try {
			const offset = (currentAlbumPage - 1) * ALBUMS_PER_PAGE;
			const q = searchQuery.trim() || undefined;
			const url = API.library.albums(ALBUMS_PER_PAGE, offset, sortBy, sortOrder, q);
			const data = await api.get<PaginatedAlbumsResponse>(url, { signal: albumsAbort.signal });
			if (id !== albumsFetchId) return;
			albums = data.albums;
			albumsTotal = data.total;
		} catch (e) {
			if (isAbortError(e)) return;
			if (id !== albumsFetchId) return;
			console.error("Couldn't load albums:", e);
			if (e instanceof ApiError) {
				error = e.message;
				errorCode = e.code;
			} else {
				error = "Couldn't load albums";
			}
		} finally {
			if (id === albumsFetchId) loadingAlbums = false;
		}
	}

	async function loadStats() {
		try {
			stats = await api.get<LibraryStats>('/api/v1/library/stats');
		} catch (e) {
			if (isAbortError(e)) return;
			console.error('Failed to load stats:', e);
		} finally {
			loadingStats = false;
		}
	}

	async function loadLibrary() {
		error = null;
		errorCode = null;
		loadingArtists = true;
		loadingStats = true;
		currentAlbumPage = 1;
		try {
			await Promise.all([recentlyAddedStore.refresh(), loadArtists(), fetchAlbums(), loadStats()]);
		} finally {
			loadingStats = false;
		}
	}

	async function syncLibrary() {
		syncing = true;
		error = null;
		errorCode = null;
		try {
			await api.global.post('/api/v1/library/sync');
			syncStatus.checkStatus();
			await loadLibrary();
		} catch (e) {
			console.error('Sync failed:', e);
			if (e instanceof ApiError) {
				error = e.message;
				errorCode = e.code;
			} else {
				error = e instanceof Error ? e.message : "Couldn't sync the library";
			}
		} finally {
			syncing = false;
		}
	}

	function handleSearchInput(): void {
		if (searchTimeout) clearTimeout(searchTimeout);
		searchTimeout = setTimeout(() => {
			currentAlbumPage = 1;
			fetchAlbums();
		}, SEARCH_DEBOUNCE_MS);
	}

	function clearSearch(): void {
		searchQuery = '';
		if (searchTimeout) clearTimeout(searchTimeout);
		currentAlbumPage = 1;
		fetchAlbums();
	}

	function handlePageChange(page: number): void {
		currentAlbumPage = page;
		fetchAlbums();
	}

	function handleSortChange(event: Event): void {
		const value = (event.target as HTMLSelectElement).value;
		const [newSortBy, newSortOrder] = value.split(':');
		sortBy = newSortBy;
		sortOrder = newSortOrder;
		currentAlbumPage = 1;
		fetchAlbums();
	}

	function convertToArtist(libArtist: LibraryArtist): Artist {
		return { title: libArtist.name, musicbrainz_id: libArtist.mbid, in_library: true };
	}

	function convertToAlbum(libAlbum: LibraryAlbum): Album {
		return {
			title: libAlbum.album,
			artist: libAlbum.artist,
			year: libAlbum.year,
			musicbrainz_id: libAlbum.foreignAlbumId || '',
			in_library: true,
			cover_url: libAlbum.cover_url
		};
	}
</script>

<div class="container mx-auto p-4 md:p-6 lg:p-8">
	{#if error}
		<div class="alert alert-error mb-6">
			<CircleX class="h-6 w-6 shrink-0" />
			<div class="flex flex-col gap-1">
				<span>{error}</span>
				{#if isConnectionError}
					<a href="/settings" class="link link-primary text-sm">Open Lidarr settings</a>
				{/if}
			</div>
			<div class="flex gap-2">
				<button
					class="btn btn-sm"
					onclick={() => {
						error = null;
						errorCode = null;
						loadLibrary();
					}}>Retry</button
				>
				<button
					class="btn btn-sm btn-circle btn-ghost"
					onclick={() => {
						error = null;
						errorCode = null;
					}}
					aria-label="Dismiss"
				>
					<X class="h-4 w-4" />
				</button>
			</div>
		</div>
	{/if}

	<div class="flex flex-col sm:flex-row justify-between items-start sm:items-center gap-4 mb-6">
		<div>
			<h1 class="text-3xl font-bold">Library</h1>
			<p class="text-base-content/70 text-sm mt-1">
				{#if loadingStats}
					<span class="skeleton h-4 w-64 inline-block"></span>
				{:else}
					{stats.artist_count} artists, {stats.album_count} albums, last sync {lastSyncText}
				{/if}
			</p>
			{#if syncFrequencyLabel}
				<p class="text-base-content/50 text-xs mt-0.5 flex items-center gap-1">
					{syncFrequencyLabel}
					<a
						href="/settings?tab=lidarr"
						class="inline-flex hover:text-base-content/70 transition-colors"
						aria-label="Configure sync frequency"
						title="Configure sync frequency"
					>
						<Settings2 class="h-3 w-3" />
					</a>
				</p>
			{/if}
		</div>
		<div class="flex gap-2">
			{#if syncStatus.isActive}
				<button
					class="btn btn-sm btn-error gap-1"
					onclick={() => syncStatus.cancelSync()}
					aria-label="Stop Sync"
				>
					<svg
						xmlns="http://www.w3.org/2000/svg"
						class="h-4 w-4"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"><rect x="3" y="3" width="18" height="18" rx="2" /></svg
					>
					<span class="hidden sm:inline">Stop sync</span>
				</button>
				{#if syncStatus.isDismissed}
					<button
						class="btn btn-sm btn-ghost gap-1"
						onclick={() => syncStatus.undismiss()}
						aria-label="Show sync progress"
					>
						<Eye class="h-4 w-4" />
						<span class="hidden sm:inline">Show progress</span>
					</button>
				{/if}
			{:else}
				<button class="btn btn-sm btn-primary gap-1" onclick={syncLibrary} disabled={syncing}>
					{#if syncing}
						<Loader2 class="h-4 w-4 animate-spin" />
						<span class="hidden sm:inline">Syncing</span>
					{:else}
						<RefreshCw class="h-4 w-4" />
						<span class="hidden sm:inline">Sync Library</span>
					{/if}
				</button>
			{/if}
		</div>
	</div>

	{#if syncStatus.isActive}
		<div class="w-full mt-2">
			<div class="flex items-center gap-2 text-sm text-base-content/70 mb-1">
				<span>{syncStatus.phaseLabel}</span>
				{#if syncStatus.totalItems > 0}
					<span>({syncStatus.processedItems}/{syncStatus.totalItems})</span>
				{/if}
			</div>
			<progress class="progress progress-primary w-full" value={syncStatus.progress} max="100"
			></progress>
		</div>
	{/if}

	{#if !isSearching}
		<section class="mb-12">
			<h2 class="text-2xl font-semibold mb-4">Recently Added</h2>
			{#if loadingRecentlyAdded}
				<div class="flex gap-4 p-4 bg-base-200 rounded-box overflow-x-auto scrollbar-hide">
					{#each Array(6) as _, i (`recently-added-skeleton-${i}`)}
						<div class="w-48 shrink-0">
							<AlbumCardSkeleton />
						</div>
					{/each}
				</div>
			{:else if recentlyAdded.artists.length > 0 || recentlyAdded.albums.length > 0}
				<div in:fade={{ duration: 300 }}>
					<HorizontalCarousel class="p-4 bg-base-200 rounded-box">
						{#each recentlyAdded.artists as artist (artist.mbid)}
							<div class="w-48 shrink-0">
								<ArtistCard artist={convertToArtist(artist)} />
							</div>
						{/each}
						{#each recentlyAdded.albums as album, i (album.foreignAlbumId || `${album.album}-${album.artist}-${i}`)}
							<div class="w-48 shrink-0">
								<AlbumCard album={convertToAlbum(album)} />
							</div>
						{/each}
					</HorizontalCarousel>
				</div>
			{:else}
				<div class="p-8 bg-base-200 rounded-box text-center text-base-content/50">
					<p>Nothing recently added yet.</p>
				</div>
			{/if}
		</section>
	{/if}

	<div class="mb-6 flex flex-col sm:flex-row gap-3">
		<div class="relative group flex-1">
			<Search
				class="absolute left-4 top-1/2 -translate-y-1/2 h-5 w-5 text-base-content/40
				group-focus-within:text-primary transition-colors duration-200 pointer-events-none"
			/>
			<input
				type="text"
				placeholder="Search albums..."
				class="input input-lg w-full rounded-full pl-11 pr-12
					bg-base-200/50 border-base-content/10
					focus:border-primary focus:bg-base-200/80
					transition-all duration-200
					placeholder:text-base-content/30"
				bind:value={searchQuery}
				oninput={handleSearchInput}
				aria-label="Search library"
			/>
			{#if isSearching}
				<button
					class="absolute right-3 top-1/2 -translate-y-1/2 btn btn-sm btn-ghost btn-circle"
					onclick={clearSearch}
					aria-label="Clear search"
				>
					<X class="h-4 w-4" />
				</button>
			{/if}
		</div>
		<select
			class="select select-bordered rounded-full"
			value="{sortBy}:{sortOrder}"
			onchange={handleSortChange}
			aria-label="Sort albums"
		>
			<option value="date_added:desc">Newest First</option>
			<option value="date_added:asc">Oldest First</option>
			<option value="title:asc">Title A-Z</option>
			<option value="title:desc">Title Z-A</option>
			<option value="artist:asc">Artist A-Z</option>
			<option value="artist:desc">Artist Z-A</option>
			<option value="year:desc">Year (Newest)</option>
			<option value="year:asc">Year (Oldest)</option>
		</select>
	</div>
	{#if isSearching && !loadingAlbums}
		<p class="text-sm text-base-content/50 mb-4 ml-4">
			{albumsTotal}
			{albumsTotal === 1 ? 'album' : 'albums'} found
		</p>
	{/if}

	{#if !isSearching}
		<section class="mb-8">
			<div class="flex justify-between items-center mb-4">
				<a
					href="/library/artists"
					class="flex items-center gap-2 hover:text-primary transition-colors group"
				>
					<h2 class="text-2xl font-semibold">Artists</h2>
					<ChevronRight class="w-5 h-5 transition-transform group-hover:translate-x-1" />
				</a>
			</div>
			{#if loadingArtists}
				<div class="flex gap-4 overflow-x-auto scrollbar-hide pb-2">
					{#each Array(8) as _, i (`artist-skeleton-${i}`)}
						<div class="w-32 sm:w-36 md:w-44 shrink-0">
							<ArtistCardSkeleton />
						</div>
					{/each}
				</div>
			{:else if artists.length > 0}
				<div in:fade={{ duration: 300 }}>
					<HorizontalCarousel class="pb-2">
						{#each artists as artist (artist.mbid)}
							<div class="w-32 sm:w-36 md:w-44 shrink-0">
								<ArtistCard artist={convertToArtist(artist)} />
							</div>
						{/each}
					</HorizontalCarousel>
				</div>
			{:else}
				<div class="p-8 bg-base-200 rounded-box text-center text-base-content/50">
					<p>No artists in library</p>
				</div>
			{/if}
		</section>
	{/if}

	<section>
		<div class="flex justify-between items-center mb-4">
			<a
				href="/library/albums"
				class="flex items-center gap-2 hover:text-primary transition-colors group"
			>
				<h2 class="text-2xl font-semibold">Albums</h2>
				<ChevronRight class="w-5 h-5 transition-transform group-hover:translate-x-1" />
			</a>
			{#if !loadingAlbums && totalAlbumPages > 1}
				<Pagination
					current={currentAlbumPage}
					total={totalAlbumPages}
					onchange={handlePageChange}
				/>
			{/if}
		</div>
		{#if loadingAlbums}
			<div
				class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
			>
				{#each Array(12) as _, i (`album-skeleton-${i}`)}
					<AlbumCardSkeleton />
				{/each}
			</div>
		{:else if albums.length > 0}
			<div
				class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
				in:fade={{ duration: 300 }}
			>
				{#each albums as album, index (album.foreignAlbumId || `${album.album}-${album.artist}-${index}`)}
					<AlbumCard album={convertToAlbum(album)} />
				{/each}
			</div>
			{#if totalAlbumPages > 1}
				<div class="flex justify-center mt-6">
					<Pagination
						current={currentAlbumPage}
						total={totalAlbumPages}
						onchange={handlePageChange}
					/>
				</div>
			{/if}
		{:else}
			<div class="p-8 bg-base-200 rounded-box text-center text-base-content/50">
				<p>{isSearching ? 'No matching albums' : 'No albums in library'}</p>
			</div>
		{/if}
	</section>

	{#if !isSearching && !loadingArtists && !loadingAlbums && artists.length === 0 && albums.length === 0}
		<div class="flex flex-col items-center justify-center min-h-50 text-center mt-8">
			<h2 class="text-2xl font-semibold mb-2">Your library is empty</h2>
			<p class="text-base-content/70 mb-4">
				Your Lidarr library is empty, or it hasn't synced yet.
			</p>
			{#if syncStatus.isActive}
				<div class="flex flex-col items-center gap-3 w-full max-w-md">
					<div class="flex gap-2">
						<button class="btn btn-error gap-2" onclick={() => syncStatus.cancelSync()}>
							<svg
								xmlns="http://www.w3.org/2000/svg"
								class="h-4 w-4"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
								stroke-linecap="round"
								stroke-linejoin="round"><rect x="3" y="3" width="18" height="18" rx="2" /></svg
							>
							Stop Sync
						</button>
						{#if syncStatus.isDismissed}
							<button
								class="btn btn-ghost gap-2"
								onclick={() => syncStatus.undismiss()}
								aria-label="Show sync progress"
							>
								<Eye class="h-4 w-4" />
								Show Progress
							</button>
						{/if}
					</div>
					<div class="w-full">
						<div class="flex items-center justify-center gap-2 text-sm text-base-content/70 mb-1">
							<span>{syncStatus.phaseLabel}</span>
							{#if syncStatus.totalItems > 0}
								<span>({syncStatus.processedItems}/{syncStatus.totalItems})</span>
							{/if}
						</div>
						<progress class="progress progress-primary w-full" value={syncStatus.progress} max="100"
						></progress>
					</div>
				</div>
			{:else}
				<button class="btn btn-primary gap-2" onclick={syncLibrary} disabled={syncing}>
					{#if syncing}
						<Loader2 class="h-4 w-4 animate-spin" />
						Syncing...
					{:else}
						Sync Now
					{/if}
				</button>
			{/if}
		</div>
	{/if}
</div>
