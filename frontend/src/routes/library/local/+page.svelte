<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { browser } from '$app/environment';
	import { playerStore } from '$lib/stores/player.svelte';
	import { playbackToast } from '$lib/stores/playbackToast.svelte';
	import { setQueueMutationToastsSuppressed } from '$lib/stores/playerUtils';
	import { deckFocus } from '$lib/stores/deckFocus.svelte';
	import { API } from '$lib/constants';
	import { api } from '$lib/api/client';
	import { withBasePath } from '$lib/utils/basePath';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import type { CrateTrack, LocalAlbumSummary, LocalAlbumMatch, CrateResponse } from '$lib/types';
	import type { QueueItem } from '$lib/player/types';
	import { launchLocalPlayback } from '$lib/player/launchLocalPlayback';
	import { buildQueueItemsFromLocal } from '$lib/player/queueHelpers';
	import { slide } from 'svelte/transition';
	import Turntable from '$lib/components/local/Turntable.svelte';
	import Crate from '$lib/components/local/Crate.svelte';
	import QueueDrawer from '$lib/components/QueueDrawer.svelte';
	import SearchCard from '$lib/components/local/SearchCard.svelte';
	import DeckVisualiser from '$lib/components/local/DeckVisualiser.svelte';
	import HorizontalCarousel from '$lib/components/HorizontalCarousel.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import {
		getLocalRecentQuery,
		getLocalAlbumsQuery,
		getLocalSuggestionsQuery,
		getLocalDecadesQuery,
		getLocalStatsQuery
	} from '$lib/queries/local/LocalQueries.svelte';
	import { ChevronDown, Headphones, Play, Shuffle, Clock } from 'lucide-svelte';

	const MBID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
	const isMbid = (id?: string | null): id is string => !!id && MBID_RE.test(id);

	let reducedMotion = $state(false);
	let queueDrawerOpen = $state(false);
	// biases the crate toward the era of whatever was last dropped on the deck
	let eraDecade = $state<number | undefined>(undefined);

	const CRATE_REFRESH_MS = 35_000;
	let crateDeadline = $state(0);
	let crateNow = $state(0);
	const countdownFraction = $derived(
		crateDeadline ? Math.max(0, Math.min(1, (crateDeadline - crateNow) / CRATE_REFRESH_MS)) : 1
	);

	// local `now` (not crateNow) avoids a reactive read that self-triggers callers in $effect
	function resetCrateCountdown() {
		const now = Date.now();
		crateNow = now;
		crateDeadline = now + CRATE_REFRESH_MS;
	}

	const recentQuery = getLocalRecentQuery();
	const rediscoverQuery = getLocalAlbumsQuery(() => ({ sort: 'rediscover', limit: 20 }));
	const suggestionsQuery = getLocalSuggestionsQuery(() => eraDecade);
	const decadesQuery = getLocalDecadesQuery();
	const statsQuery = getLocalStatsQuery();

	const crateTracks = $derived(suggestionsQuery.data?.items ?? []);
	const recentAlbums = $derived(recentQuery.data ?? []);
	const rediscoverAlbums = $derived(rediscoverQuery.data?.items ?? []);
	const decades = $derived(decadesQuery.data?.items ?? []);
	const stats = $derived(statsQuery.data ?? null);

	let openDecade = $state<number | null>(null);
	const openShelf = $derived(decades.find((d) => d.decade === openDecade) ?? null);

	const isPlaying = $derived(playerStore.isPlaying);
	const heroCover = $derived(playerStore.nowPlaying?.coverUrl ?? null);
	const upcomingCount = $derived(playerStore.upcomingQueueLength);

	// measured because the deck is content-sized, so a fixed rem value can't track it
	let deckHeight = $state(0);

	function crateToQueueItem(t: CrateTrack): QueueItem {
		return {
			trackSourceId: t.track_file_id,
			trackName: t.title,
			artistName: t.artist_name,
			trackNumber: 0,
			discNumber: 1,
			albumId: t.album_mbid ?? '',
			albumName: t.album_name,
			coverUrl: getCoverUrl(t.cover_url, t.album_mbid ?? ''),
			coverRemoteUrl: t.cover_url?.startsWith('http') ? t.cover_url : null,
			sourceType: 'local',
			streamUrl: API.stream.local(t.track_file_id),
			format: (t.format ?? '').toLowerCase()
		};
	}

	function rememberEra(t: CrateTrack) {
		if (t.year) eraDecade = Math.floor(t.year / 10) * 10;
	}

	function playCrateTrack(t: CrateTrack) {
		rememberEra(t);
		playerStore.playQueue([crateToQueueItem(t)], 0, false);
		void suggestionsQuery.refetch();
	}

	function queueCrateTrack(t: CrateTrack) {
		playerStore.addToQueue(crateToQueueItem(t));
		void suggestionsQuery.refetch();
	}

	async function playAlbum(album: LocalAlbumSummary, shuffle = false) {
		try {
			const match = await api.global.get<LocalAlbumMatch>(
				API.local.albumMatch(album.musicbrainz_id)
			);
			const tracks = [...match.tracks].sort((a, b) => a.track_number - b.track_number);
			if (!tracks.length) return;
			launchLocalPlayback(tracks, 0, shuffle, {
				albumId: album.musicbrainz_id,
				albumName: album.name,
				artistName: album.artist_name,
				coverUrl: album.cover_url ?? null,
				artistId: album.artist_mbid ?? undefined
			});
			if (album.year) eraDecade = Math.floor(album.year / 10) * 10;
		} catch {
			playbackToast.show("Couldn't load that album", 'error');
		}
	}

	// plays/queues a single track without reshuffling the crate
	function searchPlayTrack(t: CrateTrack) {
		rememberEra(t);
		playerStore.playQueue([crateToQueueItem(t)], 0, false);
	}
	function searchQueueTrack(t: CrateTrack) {
		playerStore.addToQueue(crateToQueueItem(t));
	}

	async function queueAlbum(album: LocalAlbumSummary) {
		try {
			const match = await api.global.get<LocalAlbumMatch>(
				API.local.albumMatch(album.musicbrainz_id)
			);
			const tracks = [...match.tracks].sort((a, b) => a.track_number - b.track_number);
			if (!tracks.length) return;
			const items = buildQueueItemsFromLocal(tracks, {
				albumId: album.musicbrainz_id,
				albumName: album.name,
				artistName: album.artist_name,
				coverUrl: album.cover_url ?? null,
				artistId: album.artist_mbid ?? undefined
			});
			playerStore.addMultipleToQueue(items);
		} catch {
			playbackToast.show("Couldn't queue that album", 'error');
		}
	}

	async function fetchBatch(): Promise<QueueItem[]> {
		try {
			const res = await api.global.get<CrateResponse>(API.local.suggestions(40));
			return res.items.map(crateToQueueItem);
		} catch {
			return crateTracks.map(crateToQueueItem);
		}
	}

	async function playAll() {
		const items = await fetchBatch();
		if (items.length) playerStore.playQueue(items, 0, false);
	}
	async function shuffleAll() {
		const items = await fetchBatch();
		if (items.length) playerStore.playQueue(items, 0, true);
	}
	function surprise() {
		const pool = crateTracks;
		if (!pool.length) {
			void suggestionsQuery.refetch();
			return;
		}
		const pick = pool[Math.floor(Math.random() * pool.length)];
		playCrateTrack(pick);
	}

	let heroEl: HTMLElement;
	let observer: IntersectionObserver | null = null;
	let refreshTimer: ReturnType<typeof setInterval> | null = null;

	onMount(() => {
		if (!browser) return;
		// queue mutations happen inline here (drag to Up Next, search, etc.) so the toast is redundant
		setQueueMutationToastsSuppressed(true);
		const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
		reducedMotion = mq.matches;
		const onMq = (e: MediaQueryListEvent) => (reducedMotion = e.matches);
		mq.addEventListener('change', onMq);

		observer = new IntersectionObserver(
			([entry]) => deckFocus.set(entry.isIntersecting && entry.intersectionRatio > 0.45),
			{ threshold: [0, 0.45, 0.8] }
		);
		if (heroEl) observer.observe(heroEl);

		// countdown freezes while the tab is hidden so a refresh never fires unseen
		resetCrateCountdown();
		refreshTimer = setInterval(() => {
			if (document.hidden) return;
			crateNow = Date.now();
			if (crateNow >= crateDeadline && !suggestionsQuery.isFetching) {
				resetCrateCountdown();
				void suggestionsQuery.refetch();
			}
		}, 250);

		return () => mq.removeEventListener('change', onMq);
	});

	// no self-mutating counter: that would loop the effect (keyed on dataUpdatedAt)
	$effect(() => {
		if (suggestionsQuery.dataUpdatedAt) resetCrateCountdown();
	});

	onDestroy(() => {
		setQueueMutationToastsSuppressed(false);
		observer?.disconnect();
		if (refreshTimer) clearInterval(refreshTimer);
		deckFocus.set(false);
	});

	const dimmed = $derived(isPlaying && deckFocus.inView && !reducedMotion);
</script>

<svelte:head><title>Listening Room &middot; DroppedNeedle</title></svelte:head>

<div
	class="listening-room relative isolate h-[calc(100dvh-3.5rem)] overflow-y-auto overscroll-contain {reducedMotion
		? ''
		: 'snap-y snap-proximity'}"
>
	{#if heroCover}
		<div
			class="pointer-events-none fixed inset-0 -z-10 bg-cover bg-center transition-opacity duration-1000"
			style="background-image:url({heroCover}); opacity:{dimmed
				? 0.22
				: 0.1}; filter:blur(64px) saturate(1.1);"
		></div>
	{/if}
	<div
		class="pointer-events-none fixed inset-0 -z-10 bg-gradient-to-b from-base-100/60 via-base-100/80 to-base-100"
	></div>

	<section
		bind:this={heroEl}
		class="relative isolate flex min-h-[calc(100dvh-3.5rem)] snap-start flex-col px-4 pt-5 sm:px-6 lg:px-8"
	>
		<DeckVisualiser {reducedMotion} />

		<header class="mb-4 flex items-center gap-3">
			<Headphones class="h-6 w-6 text-accent" />
			<div>
				<h1 class="text-xl font-black tracking-tight sm:text-2xl">The Listening Room</h1>
				<p class="text-xs text-base-content/50">
					{#if stats}{stats.total_tracks.toLocaleString()} tracks &middot; {stats.total_artists} artists
						&middot; {stats.total_size_human}{:else}Your local music, ready to spin{/if}
				</p>
			</div>
		</header>

		<div class="grid flex-1 grid-cols-1 items-center gap-6 lg:grid-cols-12">
			<div class="lg:col-span-7 xl:col-span-8">
				<div bind:clientHeight={deckHeight}>
					<Turntable
						onDropPlay={playCrateTrack}
						onDropAlbum={(a) => playAlbum(a)}
						onPlayAll={playAll}
						onShuffleAll={shuffleAll}
						onSurprise={surprise}
						onOpenQueue={() => (queueDrawerOpen = true)}
					/>
				</div>
			</div>
			<div
				class="flex flex-col gap-4 lg:col-span-5 lg:h-[var(--deck-h)] xl:col-span-4"
				style:--deck-h={deckHeight ? `${deckHeight}px` : '44rem'}
			>
				<div
					class="min-h-0 flex-[3] rounded-3xl border border-base-content/5 bg-base-200/30 p-3 backdrop-blur-sm"
				>
					<Crate
						tracks={crateTracks}
						isLoading={suggestionsQuery.isLoading || suggestionsQuery.isFetching}
						{reducedMotion}
						{countdownFraction}
						refreshNonce={suggestionsQuery.dataUpdatedAt ?? 0}
						refreshIntervalMs={CRATE_REFRESH_MS}
						{upcomingCount}
						onRefresh={() => suggestionsQuery.refetch()}
						onPlay={playCrateTrack}
						onQueue={queueCrateTrack}
						onQueueAlbum={queueAlbum}
					/>
				</div>
				<div
					class="min-h-0 flex-[2] rounded-3xl border border-base-content/5 bg-base-200/30 p-3 backdrop-blur-sm"
				>
					<SearchCard
						{reducedMotion}
						onPlayTrack={searchPlayTrack}
						onQueueTrack={searchQueueTrack}
						onPlayAlbum={(a) => playAlbum(a)}
						onQueueAlbum={queueAlbum}
					/>
				</div>
			</div>
		</div>

		<div class="flex items-center justify-center py-4 text-base-content/40">
			<div class="flex items-center gap-2 text-xs uppercase tracking-[0.2em]">
				Scroll for more
				<ChevronDown class="h-4 w-4 {reducedMotion ? '' : 'animate-float'}" />
			</div>
		</div>
	</section>

	<section
		class="snap-start space-y-10 px-4 pb-24 pt-6 transition-opacity duration-700 sm:px-6 lg:px-8 {dimmed
			? 'opacity-60'
			: 'opacity-100'}"
	>
		{#snippet shelf(title: string, icon: typeof Play, albums: LocalAlbumSummary[])}
			{#if albums.length}
				{@const Icon = icon}
				<div>
					<div class="mb-3 flex items-center gap-2">
						<Icon class="h-4 w-4 text-accent" />
						<h2 class="text-lg font-bold">{title}</h2>
					</div>
					<HorizontalCarousel>
						{#each albums as album (album.musicbrainz_id)}
							{@render albumTile(album)}
						{/each}
					</HorizontalCarousel>
				</div>
			{/if}
		{/snippet}

		{#snippet albumTile(album: LocalAlbumSummary)}
			<div class="group w-36 shrink-0 sm:w-40">
				<button
					class="relative block aspect-square w-full overflow-hidden rounded-xl ring-1 ring-base-content/10 transition-transform group-hover:scale-[1.03]"
					onclick={() => playAlbum(album)}
					title="Play {album.name}"
				>
					<AlbumImage
						mbid={album.musicbrainz_id}
						remoteUrl={album.cover_url}
						alt={album.name}
						size="full"
						requestSize={250}
						rounded="none"
						className="h-full w-full object-cover"
					/>
					<div
						class="absolute inset-0 flex items-center justify-center bg-base-100/40 opacity-0 backdrop-blur-[2px] transition-opacity group-hover:opacity-100"
					>
						<div class="btn btn-circle btn-primary btn-sm shadow-lg"><Play class="h-4 w-4" /></div>
					</div>
				</button>
				{#if isMbid(album.musicbrainz_id)}
					<a
						href={withBasePath(`/album/${album.musicbrainz_id}`)}
						class="mt-2 block truncate text-sm font-semibold transition-colors hover:text-accent hover:underline"
						title={album.name}>{album.name}</a
					>
				{:else}
					<p class="mt-2 truncate text-sm font-semibold">{album.name}</p>
				{/if}
				{#if isMbid(album.artist_mbid)}
					<a
						href={withBasePath(`/artist/${album.artist_mbid}`)}
						class="block truncate text-xs text-base-content/55 transition-colors hover:text-accent hover:underline"
						title={album.artist_name}>{album.artist_name}</a
					>
				{:else}
					<p class="truncate text-xs text-base-content/55">{album.artist_name}</p>
				{/if}
			</div>
		{/snippet}

		{@render shelf('Recently Added', Clock, recentAlbums)}
		{@render shelf('Rediscover', Shuffle, rediscoverAlbums)}

		{#if decades.length}
			<div>
				<div class="mb-3 flex items-center gap-2">
					<Headphones class="h-4 w-4 text-accent" />
					<h2 class="text-lg font-bold">By Decade</h2>
				</div>

				<div class="grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
					{#each decades as shelfData (shelfData.decade)}
						{@const active = openDecade === shelfData.decade}
						{@const face = shelfData.albums[0]}
						<button
							class="decade-card group relative aspect-[4/3] overflow-hidden rounded-2xl border p-4 text-left {active
								? 'border-accent/60 ring-1 ring-accent/40'
								: 'border-base-content/10 bg-base-200/40'}"
							onclick={() => (openDecade = active ? null : shelfData.decade)}
							aria-expanded={active}
							title="Browse {shelfData.label}"
						>
							{#if face}
								<img
									src={getCoverUrl(face.cover_url, face.musicbrainz_id)}
									alt=""
									aria-hidden="true"
									loading="lazy"
									class="pointer-events-none absolute inset-0 h-full w-full scale-110 object-cover opacity-20 blur-[2px] transition-opacity duration-300 group-hover:opacity-30"
								/>
							{/if}
							<div
								class="pointer-events-none absolute inset-0 bg-gradient-to-t from-base-100/85 via-base-100/40 to-transparent"
							></div>
							<ChevronDown
								class="absolute right-3 top-3 h-4 w-4 text-base-content/50 transition-transform duration-300 {active
									? 'rotate-180 text-accent'
									: ''}"
							/>
							<div class="absolute inset-x-4 bottom-3">
								<div class="text-2xl font-black tracking-tight">{shelfData.label}</div>
								<div class="text-xs text-base-content/55">{shelfData.album_count} albums</div>
							</div>
						</button>
					{/each}
				</div>

				{#if openShelf}
					<div
						class="mt-4 rounded-2xl border border-base-content/5 bg-base-200/30 p-4 backdrop-blur-sm"
						transition:slide={{ duration: reducedMotion ? 0 : 280 }}
					>
						<div class="mb-3 flex items-baseline gap-2">
							<h3 class="text-base font-bold text-base-content/85">{openShelf.label}</h3>
							<span class="text-xs text-base-content/40">{openShelf.album_count} albums</span>
						</div>
						<HorizontalCarousel>
							{#each openShelf.albums as album (album.musicbrainz_id)}
								{@render albumTile(album)}
							{/each}
						</HorizontalCarousel>
					</div>
				{/if}
			</div>
		{/if}

		{#if !recentAlbums.length && !rediscoverAlbums.length && !decades.length && !recentQuery.isLoading}
			<div class="flex flex-col items-center gap-3 py-16 text-center">
				<Headphones class="h-12 w-12 text-base-content/20" />
				<div>
					<p class="font-semibold">No local music yet</p>
					<p class="text-sm text-base-content/55">
						Add a music folder in <a
							href={withBasePath('/settings?tab=library')}
							class="link link-accent">Settings &rarr; Library</a
						>, then run a scan.
					</p>
				</div>
			</div>
		{/if}
	</section>
</div>

<QueueDrawer bind:open={queueDrawerOpen} onclose={() => (queueDrawerOpen = false)} />
