<script lang="ts">
	import { resolve } from '$app/paths';
	import { API } from '$lib/constants';
	import { api } from '$lib/api/client';
	import { buildDiscoveryQueueFromPlex } from '$lib/player/queueHelpers';
	import { playerStore } from '$lib/stores/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { createLibraryTrackLoader } from '$lib/utils/libraryTrackLoader.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import LibraryFilterBar from '$lib/components/LibraryFilterBar.svelte';
	import { formatDurationSec } from '$lib/utils/formatting';
	import { reveal } from '$lib/actions/reveal';
	import {
		ChevronLeft,
		ChevronRight,
		Play,
		Shuffle,
		ListPlus,
		ListStart,
		Loader2
	} from 'lucide-svelte';
	import type { PlexTrackInfo, PlexTrackPage } from '$lib/types';

	const PAGE_SIZE = 48;

	const TRACK_SORT_OPTIONS = [
		{ value: 'titleSort', label: 'Title' },
		{ value: 'addedAt', label: 'Recently Added' },
		{ value: 'lastViewedAt', label: 'Recently Played' },
		{ value: 'duration', label: 'Duration' }
	];

	let loading = $state(true);
	let data = $state<PlexTrackPage>({ items: [], total: 0, offset: 0, limit: PAGE_SIZE });
	let currentPage = $state(0);
	let sortBy = $state('titleSort');
	let sortOrder = $state('asc');
	let searchQuery = $state('');
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;

	const totalPages = $derived(Math.ceil(data.total / PAGE_SIZE));

	const playableTracks = $derived(data.items.filter((t) => t.part_key));

	const loader = createLibraryTrackLoader<PlexTrackInfo>(
		{
			fetchPageUrl: (limit, offset) =>
				API.plexLibrary.tracks(limit, offset, `${sortBy}:${sortOrder}`, searchQuery),
			buildQueue: (tracks) => buildDiscoveryQueueFromPlex(tracks),
			pageSize: PAGE_SIZE,
			resolveShuffleStartIndex: (tracks, requestedIndex, queue) => {
				const selectedTrack = tracks[requestedIndex];
				if (!selectedTrack?.part_key) return 0;
				const queueIndex = queue.findIndex((item) => item.trackSourceId === selectedTrack.part_key);
				return queueIndex >= 0 ? queueIndex : 0;
			}
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
			data = await api.get<PlexTrackPage>(
				API.plexLibrary.tracks(
					PAGE_SIZE,
					currentPage * PAGE_SIZE,
					`${sortBy}:${sortOrder}`,
					searchQuery
				)
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

	function handleSortChange(value: string) {
		if (value !== sortBy) {
			sortBy = value;
			sortOrder = value === 'titleSort' ? 'asc' : 'desc';
		}
		currentPage = 0;
		fetchTracks();
	}

	function toggleSortOrder() {
		sortOrder = sortOrder === 'asc' ? 'desc' : 'asc';
		currentPage = 0;
		fetchTracks();
	}

	function handleSearchInput() {
		clearTimeout(searchTimeout);
		searchTimeout = setTimeout(() => {
			currentPage = 0;
			fetchTracks();
		}, 300);
	}

	function playTrack(index: number) {
		loader.abort();
		const queue = buildDiscoveryQueueFromPlex(playableTracks);
		if (queue.length === 0) return;
		const track = playableTracks[index];
		const queueIndex = queue.findIndex((q) => q.trackSourceId === track.part_key);
		playerStore.playQueue(queue, queueIndex >= 0 ? queueIndex : 0, false);
	}

	function playAll() {
		loader.playAll(data.items, data.total);
	}

	function shuffleAll() {
		loader.shuffleAll(data.items, data.total);
	}

	function addTrackToQueue(track: PlexTrackInfo) {
		if (!track.part_key) {
			toastStore.show({ message: "This track can't be played.", type: 'error' });
			return;
		}
		const items = buildDiscoveryQueueFromPlex([track]);
		if (items.length === 0) return;
		playerStore.addMultipleToQueue(items);
		toastStore.show({ message: `"${track.title}" was added to the queue`, type: 'info' });
	}

	function playTrackNext(track: PlexTrackInfo) {
		if (!track.part_key) {
			toastStore.show({ message: "This track can't be played.", type: 'error' });
			return;
		}
		const items = buildDiscoveryQueueFromPlex([track]);
		if (items.length === 0) return;
		playerStore.playMultipleNext(items);
		toastStore.show({ message: `"${track.title}" will play next`, type: 'info' });
	}

	function getTrackMenuItems(track: PlexTrackInfo): MenuItem[] {
		return [
			{
				label: 'Add to Queue',
				icon: ListPlus,
				onclick: () => addTrackToQueue(track),
				disabled: !track.part_key
			},
			{
				label: 'Play Next',
				icon: ListStart,
				onclick: () => playTrackNext(track),
				disabled: !track.part_key
			}
		];
	}

	function isTrackPlaying(track: PlexTrackInfo): boolean {
		return (
			playerStore.isPlaying &&
			track.part_key !== null &&
			playerStore.currentQueueItem?.trackSourceId === track.part_key &&
			playerStore.currentQueueItem?.sourceType === 'plex'
		);
	}

	$effect(() => {
		fetchTracks();
		return () => loader.abort();
	});
</script>

<div class="container mx-auto max-w-7xl px-4 py-6">
	<div
		class="h-[2px] rounded-full bg-gradient-to-r from-transparent via-[rgb(var(--brand-plex))] to-transparent opacity-40 mb-6"
	></div>

	<div
		class="mb-6 rounded-xl bg-base-200/30 backdrop-blur-sm border border-base-content/5 px-5 py-4 shadow-sm flex items-center gap-3"
	>
		<a
			href={resolve('/library/plex')}
			class="btn btn-ghost btn-sm gap-1"
			aria-label="Back to Plex library"
		>
			<ChevronLeft class="h-4 w-4" />
			Back
		</a>
		<span style="color: rgb(var(--brand-plex));">
			<PlexIcon class="h-6 w-6" />
		</span>
		<h1 class="text-2xl font-bold">Plex Tracks</h1>
		{#if data.total > 0}
			<span class="badge badge-ghost">{data.total}</span>
		{/if}
	</div>

	<LibraryFilterBar
		bind:searchQuery
		onSearchInput={handleSearchInput}
		placeholder="Search tracks"
		ariaLabel="Search tracks"
		sortOptions={TRACK_SORT_OPTIONS}
		{sortBy}
		onSortChange={handleSortChange}
		{sortOrder}
		onToggleSortOrder={toggleSortOrder}
		ascValue="asc"
		resultCount={loading ? null : data.total}
		{loading}
	/>

	{#if loading}
		<div class="rounded-xl bg-base-100/40 p-5 shadow-sm overflow-x-auto">
			<div class="flex flex-col gap-3">
				{#each Array(12) as _, i (i)}
					<div class="flex items-center gap-4 py-1">
						<div class="skeleton h-4 w-8 shrink-0"></div>
						<div class="skeleton h-4 w-48"></div>
						<div class="skeleton h-4 w-32 hidden sm:block"></div>
						<div class="skeleton h-4 w-40 hidden md:block"></div>
						<div class="skeleton h-4 w-16 ml-auto shrink-0"></div>
					</div>
				{/each}
			</div>
		</div>
	{:else if data.items.length === 0}
		<div class="flex flex-col items-center justify-center py-20 text-base-content/50">
			<PlexIcon class="mb-4 h-12 w-12 opacity-20" />
			<p class="text-lg font-medium">{searchQuery ? 'No matches' : 'No tracks yet'}</p>
			<p class="mt-1 text-sm">
				{searchQuery ? 'Try another search term.' : 'Add music to Plex to see tracks here.'}
			</p>
		</div>
	{:else}
		{#if playableTracks.length > 0}
			<div class="flex items-center gap-2 mb-4">
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
		{/if}

		<div class="rounded-xl bg-base-100/40 shadow-sm overflow-hidden" use:reveal>
			<table class="table table-sm w-full table-fixed">
				<thead>
					<tr class="text-base-content/50 border-b border-base-content/5">
						<th scope="col" class="w-12 text-center">#</th>
						<th scope="col">Title</th>
						<th scope="col" class="hidden md:table-cell">Artist</th>
						<th scope="col" class="hidden lg:table-cell">Album</th>
						<th scope="col" class="w-16 text-right hidden sm:table-cell"></th>
						<th scope="col" class="w-12 text-right hidden sm:table-cell"></th>
					</tr>
				</thead>
				<tbody>
					{#each data.items as track, i (track.plex_id)}
						{@const playing = isTrackPlaying(track)}
						{@const playable = !!track.part_key}
						<tr
							class="group transition-colors {playing
								? 'bg-accent/10'
								: playable
									? 'hover:bg-base-200/50 cursor-pointer'
									: 'opacity-50'}"
							onclick={() => playable && playTrack(playableTracks.indexOf(track))}
							onkeydown={(e) =>
								playable &&
								(e.key === 'Enter' || e.key === ' ') &&
								(e.preventDefault(), playTrack(playableTracks.indexOf(track)))}
							tabindex={playable ? 0 : -1}
							role={playable ? 'button' : undefined}
							aria-label={playable ? 'Play {track.title}' : undefined}
						>
							<td class="text-center">
								<div class="flex items-center justify-center w-8 h-8">
									{#if playing}
										<NowPlayingIndicator />
									{:else if playable}
										<span class="text-base-content/40 text-sm tabular-nums group-hover:hidden"
											>{i + 1}</span
										>
										<Play class="h-3.5 w-3.5 fill-current text-primary hidden group-hover:block" />
									{:else}
										<span class="text-base-content/30 text-sm tabular-nums">{i + 1}</span>
									{/if}
								</div>
							</td>
							<td class="overflow-hidden">
								<div class="font-medium truncate text-sm {playing ? 'text-accent' : ''}">
									{track.title}
								</div>
								<div class="text-xs text-base-content/50 md:hidden truncate">
									{track.artist_name}
								</div>
							</td>
							<td class="hidden md:table-cell overflow-hidden">
								<span
									class="text-sm text-base-content/60 truncate block {playing
										? 'text-accent/70'
										: ''}">{track.artist_name}</span
								>
							</td>
							<td class="hidden lg:table-cell overflow-hidden">
								<span class="text-sm text-base-content/50 truncate block">{track.album_name}</span>
							</td>
							<td class="text-right hidden sm:table-cell">
								<span
									class="text-sm text-base-content/40 tabular-nums {playing
										? 'text-accent/60'
										: ''}"
								>
									{formatDurationSec(track.duration_seconds)}
								</span>
							</td>
							<td class="text-right hidden sm:table-cell">
								<div
									class="flex items-center justify-end gap-1 opacity-0 group-hover:opacity-100 transition-opacity"
								>
									{#if track.codec}
										<span
											class="text-[10px] font-medium text-base-content/30 tracking-wide uppercase"
										>
											{track.codec}
											{#if track.bitrate}
												<span class="ml-0.5 opacity-70">{Math.round(track.bitrate / 1000)}</span>
											{/if}
										</span>
									{/if}
									<ContextMenu items={getTrackMenuItems(track)} position="end" size="xs" />
								</div>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
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
