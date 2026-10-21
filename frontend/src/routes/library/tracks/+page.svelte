<script lang="ts">
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import { getLibraryTracksInfiniteQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import { libraryTrackParams, type LibraryTrackSort } from '$lib/queries/library/libraryTracks';
	import { createLibraryTrackQueue } from '$lib/queries/library/libraryTrackQueue.svelte';
	import { buildDiscoveryQueueFromLocal } from '$lib/player/queueHelpers';
	import { playerStore } from '$lib/stores/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import { formatDurationSec } from '$lib/utils/formatting';
	import { reveal } from '$lib/actions/reveal';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import {
		ChevronLeft,
		Play,
		Shuffle,
		ListPlus,
		ListStart,
		LoaderCircle,
		Music2,
		Search,
		X
	} from 'lucide-svelte';
	import type { NativeTrackListItem } from '$lib/types';

	const PAGE_SIZE = 48;
	const SEARCH_DEBOUNCE_MS = 300;

	let searchQuery = $state('');
	let searchTerm = $state('');
	let sort = $state<LibraryTrackSort>('recent');
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;
	$effect(() => () => clearTimeout(searchTimeout));

	const tracksQuery = getLibraryTracksInfiniteQuery(() =>
		libraryTrackParams(PAGE_SIZE, sort, searchTerm)
	);
	const pages = $derived(tracksQuery.data?.pages ?? []);
	// A track can move between pages while they load (a scan renames it):
	// keep its first appearance.
	const tracks = $derived.by(() => {
		const seen: Record<string, true> = Object.create(null);
		const out: NativeTrackListItem[] = [];
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
		searchTimeout = setTimeout(() => (searchTerm = searchQuery.trim()), SEARCH_DEBOUNCE_MS);
	}

	function clearSearch() {
		searchQuery = '';
		searchTerm = '';
		clearTimeout(searchTimeout);
	}

	function handleSortChange(e: Event) {
		sort = (e.target as HTMLSelectElement).value as LibraryTrackSort;
	}

	function playTrack(index: number) {
		queue.abort();
		const items = buildDiscoveryQueueFromLocal(tracks);
		if (items.length === 0) return;
		playerStore.playQueue(items, index, false);
	}

	// Plays what is loaded, then the rest of the list after the last page.
	function playAll() {
		void queue.playAll(sort, searchTerm, {
			items: tracks,
			total,
			next: pages.at(-1)?.next ?? null
		});
	}

	function shuffleAll() {
		void queue.shuffleAll(searchTerm);
	}

	function addTrackToQueue(track: NativeTrackListItem) {
		const items = buildDiscoveryQueueFromLocal([track]);
		if (items.length === 0) return;
		playerStore.addMultipleToQueue(items);
		toastStore.show({ message: `"${track.title}" was added to the queue`, type: 'info' });
	}

	function playTrackNext(track: NativeTrackListItem) {
		const items = buildDiscoveryQueueFromLocal([track]);
		if (items.length === 0) return;
		playerStore.playMultipleNext(items);
		toastStore.show({ message: `"${track.title}" will play next`, type: 'info' });
	}

	function getTrackMenuItems(track: NativeTrackListItem): MenuItem[] {
		return [
			{ label: 'Add to Queue', icon: ListPlus, onclick: () => addTrackToQueue(track) },
			{ label: 'Play Next', icon: ListStart, onclick: () => playTrackNext(track) }
		];
	}

	function isTrackPlaying(track: NativeTrackListItem): boolean {
		return (
			playerStore.isPlaying &&
			playerStore.currentQueueItem?.trackSourceId === track.id &&
			playerStore.currentQueueItem?.sourceType === 'local'
		);
	}
</script>

<svelte:head><title>Tracks · Library</title></svelte:head>

<div class="container mx-auto p-4 md:p-6 lg:p-8">
	<div class="flex items-center gap-4 mb-6">
		<IconButton
			icon={ChevronLeft}
			label="Back to library"
			onclick={() => goto(withBasePath('/library'))}
		/>
		<div>
			<h1 class="text-3xl font-bold">All Tracks</h1>
			<p class="text-base-content/70 text-sm mt-1">
				{total.toLocaleString()}
				{total === 1 ? 'track' : 'tracks'}
			</p>
		</div>
	</div>

	<div class="flex flex-col sm:flex-row gap-3 mb-6">
		<div class="relative flex-1">
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
		<select class="select" value={sort} onchange={handleSortChange} aria-label="Sort tracks">
			<option value="recent">Recently added</option>
			<option value="title">Title</option>
			<option value="artist">Artist</option>
			<option value="album">Album</option>
		</select>
	</div>

	<QueryBoundary
		query={tracksQuery}
		what="your tracks"
		isEmpty={(data) => !data.pages[0]?.items.length}
	>
		{#snippet loading()}
			<div class="overflow-hidden rounded-xl bg-base-100/40 shadow-sm">
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
			<div class="flex flex-col items-center justify-center py-20 text-base-content/50">
				<Music2 class="mb-4 h-12 w-12 opacity-20" />
				<p class="text-lg font-medium">{searchTerm ? 'No matches' : 'No tracks yet'}</p>
				<p class="mt-1 text-sm">
					{searchTerm ? 'Try another search term.' : 'Scan your library to see tracks here.'}
				</p>
			</div>
		{/snippet}
		<div class="mb-4 flex items-center gap-2">
			{#if queue.loading}
				<Button
					variant="primary"
					size="sm"
					onclick={() => queue.abort()}
					aria-label="Stop loading tracks"
				>
					<LoaderCircle class="h-3.5 w-3.5 animate-spin motion-keep" aria-hidden="true" />
					{queue.progressText ?? 'Loading tracks'}
				</Button>
			{:else}
				<Button
					variant="primary"
					size="sm"
					icon={Play}
					class="[&_svg]:fill-current"
					onclick={playAll}
					aria-label="Play all tracks"
				>
					Play All
				</Button>
				<Button
					variant="ghost"
					size="sm"
					icon={Shuffle}
					onclick={shuffleAll}
					aria-label="Shuffle all tracks"
				>
					Shuffle
				</Button>
			{/if}
		</div>

		<div
			class="divide-y divide-base-content/5 overflow-hidden rounded-xl bg-base-100/40 shadow-sm"
			use:reveal
		>
			{#each tracks as track, i (track.id)}
				{@const playing = isTrackPlaying(track)}
				<div
					class="group flex cursor-pointer items-center gap-3 px-3 py-2 transition-colors focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary {playing
						? 'bg-accent/10'
						: 'hover:bg-base-200/50'}"
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
							mbid={track.musicbrainz_release_group_id ?? ''}
							available={track.cover_available}
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
						<div class="truncate text-sm font-semibold {playing ? 'text-accent' : ''}">
							{track.title}
						</div>
						<div class="truncate text-xs text-base-content/55">
							{track.artist_name}{#if track.album_title}<span class="text-base-content/35">
									· {track.album_title}</span
								>{/if}
						</div>
					</div>

					<div class="flex shrink-0 items-center gap-2">
						{#if track.format}
							<span
								class="hidden text-[10px] font-medium uppercase tracking-wide text-base-content/30 sm:inline"
							>
								{track.format}
							</span>
						{/if}
						{#if track.duration_seconds != null}
							<span
								class="text-xs tabular-nums text-base-content/40 {playing ? 'text-accent/60' : ''}"
							>
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
			<div class="mt-6 flex justify-center">
				<Button
					variant="outline"
					loading={tracksQuery.isFetchingNextPage}
					onclick={() => tracksQuery.fetchNextPage()}
				>
					Load more ({tracks.length.toLocaleString()} / {total.toLocaleString()})
				</Button>
			</div>
		{/if}
	</QueryBoundary>
</div>
