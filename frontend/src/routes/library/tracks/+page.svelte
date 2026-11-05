<script lang="ts">
	import { withBasePath } from '$lib/utils/basePath';
	import { getLibraryTracksInfiniteQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import {
		libraryTrackParams,
		type LibraryTrackFilters,
		type LibraryTrackSort
	} from '$lib/features/library/queries/libraryTracks';
	import { createLibraryTrackQueue } from '$lib/features/library/queries/libraryTrackQueue.svelte';
	import { buildDiscoveryQueueFromLocal } from '$lib/features/player/playback/queueHelpers';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import NowPlayingIndicator from '$lib/features/player/components/NowPlayingIndicator.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ContextMenu from '$lib/ui/ContextMenu.svelte';
	import type { MenuItem } from '$lib/ui/ContextMenu.svelte';
	import { formatDurationSec } from '$lib/utils/formatting';
	import { reveal } from '$lib/actions/reveal';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import LibraryFormatBadge from '$lib/features/library/components/LibraryFormatBadge.svelte';
	import Input from '$lib/ui/Input.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import {
		Play,
		Shuffle,
		ListPlus,
		ListStart,
		LoaderCircle,
		Music2,
		Heart,
		Search,
		X
	} from 'lucide-svelte';
	import type { TrackView } from '$lib/features/library/queries/types';
	import LibraryFilterMenu, {
		type FormatMode,
		type RatingMode
	} from '$lib/features/library/components/LibraryFilterMenu.svelte';
	import { getLibraryStatsQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import { rememberFilters } from '$lib/features/library/state/rememberedFilters.svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';

	const PAGE_SIZE = 48;
	const SEARCH_DEBOUNCE_MS = 300;

	const SORTS: LibraryTrackSort[] = ['recent', 'title', 'artist', 'album'];

	// Browse state lives in the url, as on the album and artist lists, so
	// it can be shared, survives back and forward, and is remembered.
	const browse = $derived.by(() => {
		const sp = page.url.searchParams;
		const favorites = sp.get('favorites') === '1';
		const rawSort = sp.get('sort') as LibraryTrackSort | null;
		const valid = rawSort && (SORTS.includes(rawSort) || (favorites && rawSort === 'hearted'));
		return {
			q: sp.get('q') ?? '',
			sort: (valid ? rawSort : favorites ? 'hearted' : 'recent') as LibraryTrackSort,
			favorites,
			formats: (sp.get('formats') ?? '')
				.split(',')
				.map((format) => format.trim().toLowerCase())
				.filter(Boolean),
			formatMode: (sp.get('format_mode') === 'without' ? 'without' : 'with') as FormatMode,
			rating: Math.min(5, Math.max(0, Number(sp.get('rating')) || 0)),
			ratingMode: (sp.get('rating_mode') === 'at_most' ? 'atMost' : 'atLeast') as RatingMode
		};
	});
	const filters = $derived<LibraryTrackFilters>({
		...(browse.formats.length ? { formats: browse.formats, formatMode: browse.formatMode } : {}),
		...(browse.rating > 0
			? {
					rating: browse.rating,
					ratingMode: browse.ratingMode === 'atMost' ? ('at_most' as const) : ('at_least' as const)
				}
			: {})
	});
	const filtered = $derived(browse.formats.length > 0 || browse.rating > 0);

	// Local edits win over the url term until back or forward re-syncs it.
	let searchQuery = $derived(browse.q);
	const statsQuery = getLibraryStatsQuery();
	// The tracks of each format, lowercased like the filter.
	const formatCounts = $derived.by(() => {
		const counts: Record<string, number> = {};
		for (const [format, count] of Object.entries(statsQuery.data?.format_breakdown ?? {})) {
			const key = format.toLowerCase();
			counts[key] = (counts[key] ?? 0) + count;
		}
		return Object.entries(counts)
			.sort(([a, countA], [b, countB]) => countB - countA || a.localeCompare(b))
			.map(([format, count]) => ({ format, album_count: count }));
	});
	rememberFilters('tracks', ['formats', 'format_mode', 'rating', 'rating_mode']);
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;
	$effect(() => () => clearTimeout(searchTimeout));

	function setParams(updates: Record<string, string | null>): void {
		const url = new URL(page.url);
		for (const [key, value] of Object.entries(updates)) {
			if (!value) url.searchParams.delete(key);
			else url.searchParams.set(key, value);
		}
		void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
	}

	const tracksQuery = getLibraryTracksInfiniteQuery(() =>
		libraryTrackParams(PAGE_SIZE, browse.sort, browse.q, browse.favorites, filters)
	);
	const pages = $derived(tracksQuery.data?.pages ?? []);
	// A track can move between pages while they load (a scan renames it):
	// keep its first appearance.
	const tracks = $derived.by(() => {
		const seen: Record<string, true> = Object.create(null);
		const out: TrackView[] = [];
		for (const response of pages) {
			for (const track of response.items) {
				if (seen[track.id]) continue;
				seen[track.id] = true;
				out.push(track);
			}
		}
		return out;
	});
	const total = $derived(pages[0]?.total ?? 0);

	const queue = createLibraryTrackQueue({
		buildQueue: (items) => buildDiscoveryQueueFromLocal(items),
		playQueue: (items, startIndex, shuffle) => playerStore.playQueue(items, startIndex, shuffle),
		appendQueue: (items) => playerStore.appendQueueSilent(items),
		reshuffle: () => playerStore.regenerateShuffleOrder(),
		toast: (message, type) => toastStore.show({ message, type })
	});
	$effect(() => () => queue.abort());

	function handleSearchInput() {
		clearTimeout(searchTimeout);
		searchTimeout = setTimeout(
			() => setParams({ q: searchQuery.trim() || null }),
			SEARCH_DEBOUNCE_MS
		);
	}

	function clearSearch() {
		searchQuery = '';
		clearTimeout(searchTimeout);
		setParams({ q: null });
	}

	function handleSortChange(e: Event) {
		const sort = (e.target as HTMLSelectElement).value as LibraryTrackSort;
		setParams({ sort: sort === (browse.favorites ? 'hearted' : 'recent') ? null : sort });
	}

	function toggleFavorites() {
		setParams({ favorites: browse.favorites ? null : '1', sort: null });
	}

	function playTrack(index: number) {
		queue.abort();
		const items = buildDiscoveryQueueFromLocal(tracks);
		if (items.length === 0) return;
		playerStore.playQueue(items, index, false);
	}

	// Plays what is loaded, then the rest of the list after the last page.
	function playAll() {
		void queue.playAll(
			browse.sort,
			browse.q,
			{
				items: tracks,
				total,
				next: pages.at(-1)?.next ?? null
			},
			browse.favorites,
			filters
		);
	}

	function shuffleAll() {
		void queue.shuffleAll(browse.q, browse.favorites, filters);
	}

	function addTrackToQueue(track: TrackView) {
		const items = buildDiscoveryQueueFromLocal([track]);
		if (items.length === 0) return;
		playerStore.addMultipleToQueue(items);
		toastStore.show({ message: `"${track.title}" was added to the queue`, type: 'info' });
	}

	function playTrackNext(track: TrackView) {
		const items = buildDiscoveryQueueFromLocal([track]);
		if (items.length === 0) return;
		playerStore.playMultipleNext(items);
		toastStore.show({ message: `"${track.title}" will play next`, type: 'info' });
	}

	function getTrackMenuItems(track: TrackView): MenuItem[] {
		return [
			{ label: 'Add to Queue', icon: ListPlus, onclick: () => addTrackToQueue(track) },
			{ label: 'Play Next', icon: ListStart, onclick: () => playTrackNext(track) }
		];
	}

	function isTrackPlaying(track: TrackView): boolean {
		return (
			playerStore.isPlaying &&
			playerStore.currentQueueItem?.trackSourceId === track.id &&
			playerStore.currentQueueItem?.sourceType === 'local'
		);
	}
</script>

<svelte:head><title>Tracks · Library</title></svelte:head>

{#snippet playButtons()}
	{#if queue.loading}
		<Button
			variant="primary"
			size="display"
			onclick={() => queue.abort()}
			aria-label="Stop loading tracks"
		>
			<LoaderCircle class="size-5 animate-spin motion-keep" aria-hidden="true" />
			{queue.progressText ?? 'Loading tracks'}
		</Button>
	{:else}
		<Button
			variant="primary"
			size="display"
			icon={Play}
			onclick={playAll}
			aria-label="Play all tracks"
		>
			Play All
		</Button>
		<Button
			variant="outline"
			size="display"
			icon={Shuffle}
			onclick={shuffleAll}
			aria-label="Shuffle all tracks"
		>
			Shuffle
		</Button>
	{/if}
{/snippet}

<ListPageHeader
	title={browse.favorites ? 'Favourite Tracks' : 'All Tracks'}
	count={tracksQuery.data ? `${total.toLocaleString()} ${total === 1 ? 'track' : 'tracks'}` : null}
	ticket="DN / CC 3.2 - TRK"
	backHref={withBasePath('/library')}
	backLabel="Back to library"
	actions={tracks.length > 0 ? playButtons : undefined}
/>

<div class="px-4 pb-10 sm:px-6 lg:px-8">
	<div class="dn-toolbar">
		<div class="dn-toolbar__grow">
			<Input
				icon={Search}
				label="Search tracks"
				placeholder="Search tracks..."
				class="w-full pr-12"
				bind:value={searchQuery}
				oninput={handleSearchInput}
			/>
			{#if searchQuery}
				<IconButton
					size="sm"
					icon={X}
					label="Clear search"
					class="absolute right-2 top-1/2 -translate-y-1/2"
					onclick={clearSearch}
				/>
			{/if}
		</div>
		<Button
			variant={browse.favorites ? 'primary' : 'outline'}
			icon={Heart}
			aria-pressed={browse.favorites}
			onclick={toggleFavorites}>Favourites</Button
		>
		<select
			class="select dn-toolbar__select"
			value={browse.sort}
			onchange={handleSortChange}
			aria-label="Sort tracks"
		>
			{#if browse.favorites}<option value="hearted">Recently favourited</option>{/if}
			<option value="recent">Recently added</option>
			<option value="title">Title</option>
			<option value="artist">Artist</option>
			<option value="album">Album</option>
		</select>
		<LibraryFilterMenu
			{formatCounts}
			countLabel="tracks"
			formats={browse.formats}
			formatMode={browse.formatMode}
			onFormats={(next, mode) =>
				setParams({
					formats: next.length ? next.join(',') : null,
					format_mode: mode === 'without' && next.length ? 'without' : null
				})}
			rating={browse.rating}
			ratingMode={browse.ratingMode}
			onRating={(stars, mode) =>
				setParams({
					rating: stars > 0 ? String(stars) : null,
					rating_mode: mode === 'atMost' && stars > 0 ? 'at_most' : null
				})}
		/>
	</div>

	<QueryBoundary
		query={tracksQuery}
		what="your tracks"
		isEmpty={(data) => !data.pages[0]?.items.length}
	>
		{#snippet loading()}
			<div class="overflow-hidden rounded-xl border border-line bg-base-200">
				{#each Array(12) as _, i (i)}
					<div class="flex items-center gap-3 px-3 py-2">
						<div class="skeleton h-12 w-12 shrink-0 rounded-md"></div>
						<div class="flex-1 space-y-2">
							<div class="skeleton h-3.5 w-48"></div>
							<div class="skeleton h-3 w-32"></div>
						</div>
						<div class="skeleton h-3 w-10 shrink-0"></div>
					</div>
				{/each}
			</div>
		{/snippet}
		{#snippet empty()}
			<div class="flex flex-col items-center justify-center py-20 text-muted">
				<Music2 class="mb-4 h-12 w-12 opacity-20" />
				<p class="text-lg font-medium">
					{browse.q || filtered
						? 'No matches'
						: browse.favorites
							? 'No favourites yet'
							: 'No tracks yet'}
				</p>
				<p class="mt-1 text-sm">
					{browse.q || filtered
						? 'Try another search term or filter.'
						: browse.favorites
							? 'Tap the heart on a track to add it here.'
							: 'Scan your library to see tracks here.'}
				</p>
			</div>
		{/snippet}
		<div
			class="divide-y divide-line/60 overflow-hidden rounded-xl border border-line bg-base-200"
			use:reveal
		>
			{#each tracks as track, i (track.id)}
				{@const playing = isTrackPlaying(track)}
				<div
					class="group flex cursor-pointer items-center gap-3 px-3 py-2 transition-colors focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus {playing
						? 'bg-selected'
						: 'hover:bg-neutral'}"
					onclick={() => playTrack(i)}
					onkeydown={(e) =>
						e.target === e.currentTarget &&
						(e.key === 'Enter' || e.key === ' ') &&
						(e.preventDefault(), playTrack(i))}
					tabindex="0"
					role="button"
					aria-label="Play {track.title}"
				>
					<div class="relative h-12 w-12 shrink-0">
						<AlbumImage
							albumId={track.album_id}
							mbid={track.release_group_mbid ?? ''}
							available={track.cover_available}
							artMissing={track.art_missing}
							alt={track.album_title}
							size="full"
							requestSize={250}
							rounded="md"
							className="h-12 w-12 ring-1 ring-base-content/10"
						/>
						<div
							class="absolute inset-0 flex items-center justify-center rounded-md bg-black/45 opacity-0 transition-opacity {playing
								? 'opacity-100'
								: 'group-hover:opacity-100'}"
						>
							{#if playing}
								<NowPlayingIndicator />
							{:else}
								<Play class="h-5 w-5 fill-current text-white" />
							{/if}
						</div>
					</div>

					<div class="min-w-0 flex-1">
						<div class="flex min-w-0 items-center gap-2">
							<span class="truncate text-sm font-semibold {playing ? 'text-title' : ''}"
								>{track.title}</span
							>
							{#if playing}
								<span
									class="shrink-0 font-mono text-[0.6875rem] font-semibold tracking-widest text-title uppercase"
									>Playing</span
								>
							{/if}
						</div>
						<div class="truncate font-mono text-xs text-muted">
							{track.artist_name}{#if track.album_title}<span class="ml-1 text-muted"
									>· {track.album_title}</span
								>{/if}
						</div>
					</div>

					<div class="flex shrink-0 items-center gap-2">
						{#if track.format}
							<span class="hidden sm:inline-flex">
								<LibraryFormatBadge format={track.format} size="badge-xs" />
							</span>
						{/if}
						{#if track.duration_seconds != null}
							<span class="w-11 text-right font-mono text-xs tabular-nums text-muted">
								{formatDurationSec(track.duration_seconds)}
							</span>
						{/if}
						<div
							class="opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100"
						>
							<ContextMenu items={getTrackMenuItems(track)} position="end" size="xs" />
						</div>
					</div>
				</div>
			{/each}
		</div>

		{#if tracksQuery.hasNextPage}
			<div class="mt-8 flex justify-center">
				<Button
					variant="outline"
					class="min-w-60"
					loading={tracksQuery.isFetchingNextPage}
					onclick={() => tracksQuery.fetchNextPage()}
				>
					Load more ({tracks.length.toLocaleString()} / {total.toLocaleString()})
				</Button>
			</div>
		{/if}
	</QueryBoundary>
</div>
