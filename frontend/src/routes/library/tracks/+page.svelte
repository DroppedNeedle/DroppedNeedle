<script lang="ts">
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import { API } from '$lib/constants';
	import { api } from '$lib/api/client';
	import { buildDiscoveryQueueFromLocal } from '$lib/player/queueHelpers';
	import { playerStore } from '$lib/stores/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { createLibraryTrackLoader } from '$lib/utils/libraryTrackLoader.svelte';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import { formatDurationSec } from '$lib/utils/formatting';
	import { reveal } from '$lib/actions/reveal';
	import {
		ChevronLeft,
		ChevronRight,
		Play,
		Shuffle,
		ListPlus,
		ListStart,
		Loader2,
		Music2,
		Search,
		X
	} from 'lucide-svelte';
	import type { NativeTrackListItem, NativeTrackPage, TrackSort } from '$lib/types';
	import { untrack } from 'svelte';

	const PAGE_SIZE = 48;

	let loading = $state(true);
	let data = $state<NativeTrackPage>({ items: [], total: 0, offset: 0, limit: PAGE_SIZE });
	let currentPage = $state(0);
	let searchQuery = $state('');
	let sort = $state<TrackSort>('recent');
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;

	const totalPages = $derived(Math.ceil(data.total / PAGE_SIZE));

	const loader = createLibraryTrackLoader<NativeTrackListItem>(
		{
			fetchPageUrl: (limit, offset) => API.library.tracks(limit, offset, sort, searchQuery),
			buildQueue: (tracks) => buildDiscoveryQueueFromLocal(tracks),
			pageSize: PAGE_SIZE
		},
		(items) => playerStore.appendQueueSilent(items),
		(items, startIndex, shuffle) => playerStore.playQueue(items, startIndex, shuffle),
		() => playerStore.regenerateShuffleOrder(),
		(message, type) => toastStore.show({ message, type })
	);

	async function fetchTracks() {
		loader.abort();
		loading = true;
		try {
			data = await api.global.get<NativeTrackPage>(
				API.library.tracks(PAGE_SIZE, currentPage * PAGE_SIZE, sort, searchQuery)
			);
		} catch {
			data = { items: [], total: 0, offset: 0, limit: PAGE_SIZE };
		} finally {
			loading = false;
		}
	}

	function goToPage(page: number) {
		currentPage = page;
		fetchTracks();
		window.scrollTo({ top: 0, behavior: 'smooth' });
	}

	function handleSearchInput() {
		clearTimeout(searchTimeout);
		searchTimeout = setTimeout(() => {
			currentPage = 0;
			fetchTracks();
		}, 300);
	}

	function clearSearch() {
		searchQuery = '';
		clearTimeout(searchTimeout);
		currentPage = 0;
		fetchTracks();
	}

	function handleSortChange(e: Event) {
		sort = (e.target as HTMLSelectElement).value as TrackSort;
		currentPage = 0;
		fetchTracks();
	}

	function playTrack(index: number) {
		loader.abort();
		const queue = buildDiscoveryQueueFromLocal(data.items);
		if (queue.length === 0) return;
		playerStore.playQueue(queue, index, false);
	}

	function playAll() {
		loader.playAll(data.items, data.total);
	}

	function shuffleAll() {
		loader.shuffleAll(data.items, data.total);
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

	// untrack stops this mount-fetch re-running on state changes (handlers refetch), avoiding double-fetch
	$effect(() => {
		untrack(() => fetchTracks());
		return () => loader.abort();
	});
</script>

<svelte:head><title>Tracks · Library</title></svelte:head>

<div class="container mx-auto p-4 md:p-6 lg:p-8">
	<div class="flex items-center gap-4 mb-6">
		<button
			class="btn btn-ghost btn-circle"
			onclick={() => goto(withBasePath('/library'))}
			aria-label="Back to library"
		>
			<ChevronLeft class="w-6 h-6" />
		</button>
		<div>
			<h1 class="text-3xl font-bold">All Tracks</h1>
			<p class="text-base-content/70 text-sm mt-1">
				{data.total.toLocaleString()}
				{data.total === 1 ? 'track' : 'tracks'}
			</p>
		</div>
	</div>

	<div class="flex flex-col sm:flex-row gap-3 mb-6">
		<div class="relative group flex-1">
			<Search
				class="absolute left-4 top-1/2 -translate-y-1/2 h-5 w-5 text-base-content/40
				group-focus-within:text-primary transition-colors duration-200 pointer-events-none"
			/>
			<input
				type="text"
				placeholder="Search tracks..."
				class="input input-bordered w-full rounded-full pl-11 pr-12"
				bind:value={searchQuery}
				oninput={handleSearchInput}
				aria-label="Search tracks"
			/>
			{#if searchQuery}
				<button
					type="button"
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
			value={sort}
			onchange={handleSortChange}
			aria-label="Sort tracks"
		>
			<option value="recent">Recently added</option>
			<option value="title">Title</option>
			<option value="artist">Artist</option>
			<option value="album">Album</option>
		</select>
	</div>

	{#if loading}
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
	{:else if data.items.length === 0}
		<div class="flex flex-col items-center justify-center py-20 text-base-content/50">
			<Music2 class="mb-4 h-12 w-12 opacity-20" />
			<p class="text-lg font-medium">{searchQuery ? 'No matches' : 'No tracks yet'}</p>
			<p class="mt-1 text-sm">
				{searchQuery ? 'Try another search term.' : 'Scan your library to see tracks here.'}
			</p>
		</div>
	{:else}
		<div class="mb-4 flex items-center gap-2">
			{#if loader.loading}
				<button
					class="btn btn-sm btn-primary gap-1.5"
					onclick={() => loader.abort()}
					aria-busy="true"
					aria-label="Stop loading tracks"
				>
					<Loader2 class="h-3.5 w-3.5 animate-spin" />
					{loader.progressText ?? 'Loading tracks'}
				</button>
			{:else}
				<button
					class="btn btn-sm btn-primary gap-1.5"
					onclick={playAll}
					aria-label="Play all tracks"
				>
					<Play class="h-3.5 w-3.5 fill-current" />
					Play All
				</button>
				<button
					class="btn btn-sm btn-ghost gap-1.5"
					onclick={shuffleAll}
					aria-label="Shuffle all tracks"
				>
					<Shuffle class="h-3.5 w-3.5" />
					Shuffle
				</button>
			{/if}
		</div>

		<div
			class="divide-y divide-base-content/5 overflow-hidden rounded-xl bg-base-100/40 shadow-sm"
			use:reveal
		>
			{#each data.items as track, i (track.id)}
				{@const playing = isTrackPlaying(track)}
				<div
					class="group flex cursor-pointer items-center gap-3 px-3 py-2 transition-colors {playing
						? 'bg-accent/10'
						: 'hover:bg-base-200/50'}"
					onclick={() => playTrack(i)}
					onkeydown={(e) =>
						(e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), playTrack(i))}
					tabindex="0"
					role="button"
					aria-label="Play {track.title}"
				>
					<div class="relative h-12 w-12 shrink-0">
						<AlbumImage
							mbid={track.musicbrainz_release_group_id ?? ''}
							source="local"
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
						<!-- svelte-ignore a11y_no_static_element_interactions -->
						<div
							class="opacity-0 transition-opacity group-hover:opacity-100"
							onclick={(e) => e.stopPropagation()}
							onkeydown={(e) => e.stopPropagation()}
						>
							<ContextMenu items={getTrackMenuItems(track)} position="end" size="xs" />
						</div>
					</div>
				</div>
			{/each}
		</div>

		{#if totalPages > 1}
			<div class="mt-8 flex items-center justify-center gap-2">
				<button
					class="btn btn-ghost btn-sm"
					disabled={currentPage === 0}
					onclick={() => goToPage(currentPage - 1)}
					aria-label="Previous page"
				>
					<ChevronLeft class="h-4 w-4" />
				</button>
				<span class="text-sm text-base-content/70">
					Page {currentPage + 1} of {totalPages}
				</span>
				<button
					class="btn btn-ghost btn-sm"
					disabled={currentPage >= totalPages - 1}
					onclick={() => goToPage(currentPage + 1)}
					aria-label="Next page"
				>
					<ChevronRight class="h-4 w-4" />
				</button>
			</div>
		{/if}
	{/if}
</div>
