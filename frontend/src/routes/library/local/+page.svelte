<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import { browser } from '$app/environment';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { setQueueMutationToastsSuppressed } from '$lib/features/player/state/playerUtils';
	import { deckFocus } from '$lib/features/player/state/deckFocus.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { formatBytes } from '$lib/utils/formatting';
	import type { CrateTrack } from '$lib/features/library/queries/types';
	import type { QueueItem } from '$lib/features/player/playback/types';
	import { slide } from '$lib/ui/motion';
	import Turntable from '$lib/features/library/components/local/Turntable.svelte';
	import Crate from '$lib/features/library/components/local/Crate.svelte';
	import QueueDrawer from '$lib/features/player/components/QueueDrawer.svelte';
	import SearchCard from '$lib/features/library/components/local/SearchCard.svelte';
	import HorizontalCarousel from '$lib/ui/HorizontalCarousel.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import NoFilesBadge from '$lib/features/library/components/NoFilesBadge.svelte';
	import ContextMenu, { menuTrigger, type MenuPoint } from '$lib/ui/ContextMenu.svelte';
	import {
		getLocalRecentV3Query,
		getLocalAlbumsV3Query,
		getLocalSuggestionsV3Query,
		getLocalDecadesV3Query,
		getLocalStatsV3Query,
		fetchLocalAlbumTracksV3,
		fetchLocalSuggestionBatchV3,
		type AlbumCardV3
	} from '$lib/features/library/queries/LocalV3Queries.svelte';
	import {
		albumCoverUrl,
		crateCoverUrl,
		matchTrackToQueueItem,
		suggestionToCrateTrack,
		type SpinAlbum
	} from '$lib/features/library/queries/LocalV3Adapters';
	import { ChevronDown, Headphones, ListPlus, ListStart, Play } from 'lucide-svelte';

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

	const recentQuery = getLocalRecentV3Query(() => 20);
	const rediscoverQuery = getLocalAlbumsV3Query(() => ({
		limit: 20,
		sort: 'rediscover',
		order: 'asc'
	}));
	const suggestionsQuery = getLocalSuggestionsV3Query(() => eraDecade);
	const decadesQuery = getLocalDecadesV3Query();
	const statsQuery = getLocalStatsV3Query();

	let openDecade = $state<number | null>(null);
	const shelfQuery = getLocalAlbumsV3Query(
		() => ({ limit: 50, sort: 'name', order: 'asc', decade: openDecade ?? undefined }),
		() => openDecade !== null
	);

	const recentAlbums = $derived(recentQuery.data ?? []);
	const rediscoverAlbums = $derived(rediscoverQuery.data?.items ?? []);
	const decades = $derived(decadesQuery.data?.items ?? []);
	const stats = $derived(statsQuery.data ?? null);
	const plateStats = $derived(
		stats
			? {
					tracks: stats.total_tracks,
					artists: stats.total_artists,
					size: formatBytes(stats.total_size_bytes)
				}
			: null
	);
	const shelfAlbums = $derived(shelfQuery.data?.items ?? []);
	const openShelf = $derived.by(() => {
		const shelf = decades.find((d) => d.decade === openDecade) ?? null;
		if (!shelf) return null;
		return { ...shelf, albums: shelfAlbums };
	});

	// Face covers for closed decade cards, from the shelves already on
	// screen. Decades with no card in either shelf render without art until
	// opened, when the browse query supplies the face.
	const faceByDecade = $derived.by(() => {
		const faces = new SvelteMap<number, AlbumCardV3>();
		for (const card of [...(recentQuery.data ?? []), ...(rediscoverQuery.data?.items ?? [])]) {
			if (card.year == null) continue;
			const decade = Math.floor(card.year / 10) * 10;
			if (!faces.has(decade)) faces.set(decade, card);
		}
		return faces;
	});

	// Suggestion tracks carry the owning album's local id but no mbid, so
	// covers resolve through the album cards already on screen.
	const albumMbidById = $derived.by(() => {
		const map = new SvelteMap<string, string | null>();
		for (const card of [
			...(recentQuery.data ?? []),
			...(rediscoverQuery.data?.items ?? []),
			...(shelfQuery.data?.items ?? [])
		]) {
			map.set(card.id, card.release_group_mbid ?? null);
		}
		return map;
	});
	const crateTracks = $derived(
		(suggestionsQuery.data?.items ?? []).map((s) =>
			suggestionToCrateTrack(s, albumMbidById.get(s.album_id) ?? null)
		)
	);

	const isPlaying = $derived(playerStore.isPlaying);
	const upcomingCount = $derived(playerStore.upcomingQueueLength);

	// measured because the deck is content-sized, so a fixed rem value can't track it
	let deckHeight = $state(0);

	function crateToQueueItem(t: CrateTrack): QueueItem {
		return {
			trackSourceId: t.track_file_id,
			trackName: t.title,
			artistName: t.artist_name,
			artistId: t.artist_id ?? undefined,
			trackNumber: 0,
			discNumber: 1,
			albumId: t.album_mbid ?? t.album_id,
			albumName: t.album_name,
			coverUrl: crateCoverUrl(t),
			sourceType: 'local',
			format: (t.format ?? '').toLowerCase(),
			duration: t.duration_seconds ?? undefined
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

	function playCrateNext(t: CrateTrack) {
		playerStore.playNext(crateToQueueItem(t));
		void suggestionsQuery.refetch();
	}

	async function playAlbum(album: SpinAlbum, shuffle = false) {
		try {
			const items = await albumQueueItems(album);
			if (!items.length) return;
			playerStore.playQueue(items, 0, shuffle);
			if (album.year) eraDecade = Math.floor(album.year / 10) * 10;
		} catch {
			toastStore.notify("Couldn't load that album", 'error');
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

	async function albumQueueItems(album: SpinAlbum): Promise<QueueItem[]> {
		const match = await fetchLocalAlbumTracksV3(album.id);
		const tracks = [...match.items].sort(
			(a, b) => a.disc_number - b.disc_number || a.track_number - b.track_number
		);
		const cover = albumCoverUrl(album);
		return tracks.map((t) => matchTrackToQueueItem(t, cover));
	}

	async function queueAlbum(album: SpinAlbum) {
		try {
			const items = await albumQueueItems(album);
			if (items.length) playerStore.addMultipleToQueue(items);
		} catch {
			toastStore.notify("Couldn't queue that album", 'error');
		}
	}

	async function playAlbumNext(album: SpinAlbum) {
		try {
			const items = await albumQueueItems(album);
			if (items.length) playerStore.playMultipleNext(items);
		} catch {
			toastStore.notify("Couldn't queue that album", 'error');
		}
	}

	// one menu for every album tile, opened from a tile
	let albumMenu = $state<(MenuPoint & { album: AlbumCardV3 }) | null>(null);

	function albumMenuItems(album: AlbumCardV3) {
		return [
			{ label: 'Play', icon: Play, onclick: () => void playAlbum(album) },
			{ label: 'Play Next', icon: ListStart, onclick: () => void playAlbumNext(album) },
			{ label: 'Add to Queue', icon: ListPlus, onclick: () => void queueAlbum(album) }
		];
	}

	async function fetchBatch(): Promise<QueueItem[]> {
		try {
			const res = await fetchLocalSuggestionBatchV3();
			return res.items.map((s) =>
				crateToQueueItem(suggestionToCrateTrack(s, albumMbidById.get(s.album_id) ?? null))
			);
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

<div class="listening-room px-4 pt-4 pb-16 sm:px-6 sm:pt-6 lg:px-8">
	<div
		class="grid grid-cols-1 gap-5 lg:grid-cols-[minmax(0,1fr)_20rem] 2xl:grid-cols-[minmax(0,1fr)_24rem]"
	>
		<div bind:this={heroEl} bind:clientHeight={deckHeight} class="min-w-0 self-start">
			<Turntable
				stats={plateStats}
				{reducedMotion}
				onDropPlay={playCrateTrack}
				onDropAlbum={(a) => playAlbum(a)}
				onPlayAll={playAll}
				onShuffleAll={shuffleAll}
				onSurprise={surprise}
				onOpenQueue={() => (queueDrawerOpen = true)}
			/>
		</div>
		<div
			class="flex flex-col gap-4 lg:h-[var(--deck-h)]"
			style:--deck-h={deckHeight ? `${deckHeight}px` : '44rem'}
		>
			<div class="h-[30rem] min-h-0 lg:h-auto lg:flex-[5]">
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
					onPlayNext={playCrateNext}
					onQueueAlbum={queueAlbum}
				/>
			</div>
			<div class="h-[22rem] min-h-0 lg:h-auto lg:flex-[4]">
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

	<div
		class="mt-12 space-y-12 transition-opacity duration-700 {dimmed ? 'opacity-60' : 'opacity-100'}"
	>
		{#snippet shelf(title: string, albums: AlbumCardV3[])}
			{#if albums.length}
				<section>
					<SectionHead {title} />
					<HorizontalCarousel>
						{#each albums as album, i (`${album.id}:${i}`)}
							{@render albumTile(album)}
						{/each}
					</HorizontalCarousel>
				</section>
			{/if}
		{/snippet}

		{#snippet albumTile(album: AlbumCardV3)}
			<div
				class="w-36 shrink-0 sm:w-40 lg:w-44"
				use:menuTrigger={{
					enabled: album.track_count > 0,
					open: (at) => (albumMenu = { ...at, album }),
					close: () => (albumMenu = null)
				}}
			>
				<MediaTile
					title={album.title}
					href={isMbid(album.release_group_mbid)
						? withBasePath(`/album/${album.release_group_mbid}`)
						: undefined}
					class="w-full"
				>
					{#snippet art()}
						<AlbumImage
							customUrl={albumCoverUrl(album)}
							alt={album.title}
							size="full"
							requestSize={250}
							rounded="md"
							className="h-full w-full object-cover"
						/>
					{/snippet}
					{#snippet overlay()}
						{#if album.track_count === 0}
							<NoFilesBadge className="absolute right-2 top-2" />
						{:else}
							<IconButton
								variant="primary"
								size="sm"
								icon={Play}
								label="Play {album.title}"
								class="tile-play absolute right-2 bottom-2 shadow-lg"
								onclick={() => playAlbum(album)}
							/>
						{/if}
					{/snippet}
				</MediaTile>
				{#if isMbid(album.artist_mbid)}
					<a href={withBasePath(`/artist/${album.artist_mbid}`)} class="tile-artist">
						{album.artist_name}
					</a>
				{:else}
					<p class="tile-artist">{album.artist_name}</p>
				{/if}
			</div>
		{/snippet}

		{@render shelf('Recently Added', recentAlbums)}
		{@render shelf('Rediscover', rediscoverAlbums)}

		{#if decades.length}
			<section>
				<SectionHead title="By Decade" />
				<div class="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4 2xl:grid-cols-6">
					{#each decades as shelfData (shelfData.decade)}
						{@const active = openDecade === shelfData.decade}
						{@const face =
							active && shelfAlbums.length ? shelfAlbums[0] : faceByDecade.get(shelfData.decade)}
						{@const faceCover = face ? albumCoverUrl(face) : null}
						<button
							type="button"
							class="decade-card"
							class:is-open={active}
							onclick={() => (openDecade = active ? null : shelfData.decade)}
							aria-expanded={active}
							title="Browse {shelfData.label}"
						>
							<span class="decade-card__label">{shelfData.label}</span>
							<span class="decade-card__count">{shelfData.album_count} albums</span>
							<ChevronDown class="decade-card__chevron" aria-hidden="true" />
							{#if faceCover}
								<span class="decade-card__sleeve" aria-hidden="true">
									<AlbumImage
										customUrl={faceCover}
										alt=""
										size="full"
										requestSize={250}
										rounded="none"
										showPlaceholder={false}
										className="h-full w-full object-cover"
									/>
								</span>
							{/if}
						</button>
					{/each}
				</div>

				{#if openShelf}
					<div class="decade-shelf" transition:slide={{ duration: reducedMotion ? 0 : 280 }}>
						<SectionHead title={openShelf.label} as="h3">
							{#snippet chips()}
								<span class="decade-card__count">{openShelf.album_count} albums</span>
							{/snippet}
						</SectionHead>
						{#if shelfQuery.isLoading}
							<div class="flex gap-4 overflow-hidden">
								{#each Array(6) as _, i (`shelf-skeleton-${i}`)}
									<div class="w-36 shrink-0 sm:w-40 lg:w-44">
										<div class="skeleton aspect-square w-full rounded-md"></div>
										<div class="skeleton mt-2.5 h-3.5 w-3/4"></div>
										<div class="skeleton mt-1.5 h-3 w-1/2"></div>
									</div>
								{/each}
							</div>
						{:else}
							<HorizontalCarousel>
								{#each openShelf.albums as album, i (`${album.id}:${i}`)}
									{@render albumTile(album)}
								{/each}
							</HorizontalCarousel>
						{/if}
					</div>
				{/if}
			</section>
		{/if}

		{#if !recentAlbums.length && !rediscoverAlbums.length && !decades.length && !recentQuery.isLoading}
			<div class="flex flex-col items-center gap-3 py-16 text-center">
				<Headphones class="size-12 text-muted" aria-hidden="true" />
				<div>
					<p class="font-semibold">No local music yet</p>
					<p class="text-sm text-muted">
						Add a music folder in <a
							href={withBasePath('/settings?tab=library')}
							class="link link-accent">Settings &rarr; Library</a
						>, then run a scan.
					</p>
				</div>
			</div>
		{/if}
	</div>
</div>

{#if albumMenu}
	<ContextMenu
		trigger="point"
		at={albumMenu}
		label="{albumMenu.album.title} by {albumMenu.album.artist_name}"
		items={albumMenuItems(albumMenu.album)}
		onclose={() => (albumMenu = null)}
	/>
{/if}

<QueueDrawer bind:open={queueDrawerOpen} onclose={() => (queueDrawerOpen = false)} />

<style>
	.tile-artist {
		display: block;
		margin-top: -0.125rem;
		padding-inline: 0.25rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		line-height: 1.35;
		color: var(--color-muted);
	}
	a.tile-artist {
		border-radius: 0.25rem;
		transition: color 150ms;
	}
	a.tile-artist:hover {
		color: var(--dn-title);
		text-decoration: underline;
	}

	/* on a mouse the play button shows on hover and focus; on touch it stays */
	@media (hover: hover) {
		.listening-room :global(.tile-play) {
			opacity: 0;
			transition: opacity 150ms;
		}
		.listening-room :global(.dn-tile:hover .tile-play),
		.listening-room :global(.tile-play:focus-visible) {
			opacity: 1;
		}
	}

	/* a decade is a crate divider: the years big, the count in mono, one sleeve standing up */
	.decade-card {
		position: relative;
		display: flex;
		min-height: 8.5rem;
		flex-direction: column;
		align-items: flex-start;
		justify-content: flex-end;
		overflow: hidden;
		padding: 1rem;
		border: 1px solid var(--dn-line);
		border-radius: 0.875rem;
		background-color: var(--color-base-200);
		text-align: left;
		transition:
			border-color 150ms,
			background-color 150ms,
			translate 200ms var(--ease-spring);
	}
	.decade-card:hover {
		border-color: var(--dn-edge);
		translate: 0 -2px;
	}
	.decade-card:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: 2px;
	}
	.decade-card.is-open {
		border-color: var(--color-primary);
		background-color: var(--dn-selected);
	}
	.decade-card__label {
		position: relative;
		z-index: 1;
		font-family: var(--font-display);
		font-size: 2.25rem;
		line-height: 1;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		font-synthesis: none;
		color: var(--dn-title);
	}
	.decade-card__count {
		position: relative;
		z-index: 1;
		margin-top: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--color-muted);
	}
	.decade-card :global(.decade-card__chevron) {
		position: absolute;
		top: 0.875rem;
		left: 1rem;
		width: 1.125rem;
		height: 1.125rem;
		color: var(--color-muted);
		transition: rotate 250ms var(--ease-spring);
	}
	.decade-card.is-open :global(.decade-card__chevron) {
		rotate: 180deg;
		color: var(--color-primary);
	}
	.decade-card__sleeve {
		position: absolute;
		top: 0.875rem;
		right: 0.875rem;
		width: 4.25rem;
		height: 4.25rem;
		overflow: hidden;
		border-radius: 0.25rem;
		rotate: 5deg;
		box-shadow:
			0 0 0 1px color-mix(in oklab, var(--color-base-content) 14%, transparent),
			0 10px 18px -10px rgb(0 0 0 / 0.7);
		transition: rotate 200ms var(--ease-spring);
	}
	@media (max-width: 639px) {
		.decade-card {
			min-height: 7.5rem;
			padding: 0.875rem;
		}
		.decade-card__label {
			font-size: 1.875rem;
		}
		.decade-card__sleeve {
			width: 3.25rem;
			height: 3.25rem;
			top: 0.75rem;
			right: 0.75rem;
		}
	}
	.decade-card:hover .decade-card__sleeve {
		rotate: 1deg;
	}

	.decade-shelf {
		margin-top: 1rem;
		padding: 1.25rem 1.25rem 0.5rem;
		border: 1px solid var(--dn-line);
		border-radius: 1rem;
		background-color: var(--color-base-200);
	}
</style>
