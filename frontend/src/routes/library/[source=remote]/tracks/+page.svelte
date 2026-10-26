<script lang="ts">
	import { page } from '$app/state';
	import { untrack } from 'svelte';
	import Button from '$lib/ui/Button.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import {
		fetchRemoteTracksPage,
		getRemoteTracksQuery
	} from '$lib/queries/remotes/RemoteQueries.svelte';
	import { defaultSortOrder } from '$lib/queries/remotes/remoteSources';
	import type { RemoteTrack } from '$lib/queries/remotes/types';
	import { playerStore } from '$lib/stores/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { createLibraryTrackLoader } from '$lib/utils/libraryTrackLoader.svelte';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import LibraryFilterBar from '$lib/components/LibraryFilterBar.svelte';
	import { formatDurationSec } from '$lib/utils/formatting';
	import { reveal } from '$lib/actions/reveal';
	import { Play, Shuffle, ListPlus, ListStart, LoaderCircle } from 'lucide-svelte';
	import ListHeader from '../ListHeader.svelte';
	import Pager from '../Pager.svelte';
	import type { PageData } from './$types';

	const PAGE_SIZE = 48;

	let { data }: { data: PageData } = $props();
	const remote = $derived(data.remote);
	// The layout remounts the page for another server, so its first sort stands.
	const start = untrack(() => data.remote);
	const sorts = $derived(remote.sorts.tracks);

	const initialSearch = page.url.searchParams.get('search') ?? '';
	let currentPage = $state(0);
	let sortBy = $state(start.sorts.tracks?.[0]?.value ?? '');
	let sortOrder = $state<'asc' | 'desc'>('asc');
	let searchQuery = $state(initialSearch);
	let search = $state(initialSearch);
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;

	const listParams = $derived({
		search,
		...(sorts ? { sort_by: sortBy, sort_order: sortOrder } : {})
	});
	const tracksQuery = getRemoteTracksQuery(
		() => remote.source,
		() => ({ ...listParams, limit: PAGE_SIZE, offset: currentPage * PAGE_SIZE })
	);
	const items = $derived(tracksQuery.data?.items ?? []);
	const total = $derived(tracksQuery.data?.total ?? 0);
	const totalPages = $derived(Math.ceil(total / PAGE_SIZE));
	const playableTracks = $derived(items.filter((t) => remote.playKey(t) !== null));

	const loader = createLibraryTrackLoader<RemoteTrack>(
		{
			fetchPage: async (limit, offset, signal) =>
				fetchRemoteTracksPage(remote.source, { ...listParams, limit, offset }, signal),
			buildQueue: (tracks) => remote.queue(tracks),
			pageSize: PAGE_SIZE,
			resolveShuffleStartIndex: (tracks, requestedIndex, queue) => {
				const key = tracks[requestedIndex] ? remote.playKey(tracks[requestedIndex]) : null;
				const queueIndex = key ? queue.findIndex((item) => item.trackSourceId === key) : -1;
				return queueIndex >= 0 ? queueIndex : 0;
			}
		},
		(queued) => playerStore.appendQueueSilent(queued),
		(queued, startIndex, shuffle) => playerStore.playQueue(queued, startIndex, shuffle),
		() => playerStore.regenerateShuffleOrder(),
		(message, type) => toastStore.show({ message, type })
	);

	// Another page, sort or search stops a background play-all load.
	$effect(() => {
		void listParams;
		void currentPage;
		return () => loader.abort();
	});

	function goToPage(next: number) {
		currentPage = next;
		window.scrollTo({ top: 0, behavior: 'smooth' });
	}

	function handleSortChange(value: string) {
		if (value !== sortBy && sorts) {
			sortBy = value;
			sortOrder = defaultSortOrder(sorts, value);
		}
		currentPage = 0;
	}

	function toggleSortOrder() {
		sortOrder = sortOrder === 'asc' ? 'desc' : 'asc';
		currentPage = 0;
	}

	function handleSearchInput() {
		clearTimeout(searchTimeout);
		searchTimeout = setTimeout(() => {
			currentPage = 0;
			search = searchQuery;
		}, 300);
	}

	function playTrack(track: RemoteTrack) {
		loader.abort();
		const queue = remote.queue(playableTracks);
		if (queue.length === 0) return;
		const queueIndex = queue.findIndex((q) => q.trackSourceId === remote.playKey(track));
		playerStore.playQueue(queue, queueIndex >= 0 ? queueIndex : 0, false);
	}

	function queueTrack(track: RemoteTrack, next: boolean) {
		if (remote.playKey(track) === null) {
			toastStore.show({ message: "This track can't be played.", type: 'error' });
			return;
		}
		const queued = remote.queue([track]);
		if (queued.length === 0) return;
		if (next) {
			playerStore.playMultipleNext(queued);
			toastStore.show({ message: `"${track.title}" will play next`, type: 'info' });
		} else {
			playerStore.addMultipleToQueue(queued);
			toastStore.show({ message: `"${track.title}" was added to the queue`, type: 'info' });
		}
	}

	function getTrackMenuItems(track: RemoteTrack): MenuItem[] {
		const disabled = remote.playKey(track) === null;
		return [
			{ label: 'Add to Queue', icon: ListPlus, onclick: () => queueTrack(track, false), disabled },
			{ label: 'Play Next', icon: ListStart, onclick: () => queueTrack(track, true), disabled }
		];
	}

	function isTrackPlaying(track: RemoteTrack): boolean {
		const key = remote.playKey(track);
		return (
			playerStore.isPlaying &&
			key !== null &&
			playerStore.currentQueueItem?.trackSourceId === key &&
			playerStore.currentQueueItem?.sourceType === remote.source
		);
	}
</script>

<div class="container mx-auto max-w-7xl px-4 py-6">
	<ListHeader {remote} kind="Tracks" {total} />

	<LibraryFilterBar
		bind:searchQuery
		onSearchInput={handleSearchInput}
		placeholder="Search tracks"
		ariaLabel="Search tracks"
		sortOptions={sorts ?? undefined}
		sortBy={sorts ? sortBy : undefined}
		onSortChange={sorts ? handleSortChange : undefined}
		sortOrder={sorts ? sortOrder : undefined}
		onToggleSortOrder={sorts ? toggleSortOrder : undefined}
		ascValue="asc"
		resultCount={tracksQuery.isPending ? null : total}
		loading={tracksQuery.isPending}
	/>

	<QueryBoundary query={tracksQuery} what="{remote.label} tracks">
		{#snippet loading()}
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
		{/snippet}
		{#snippet empty()}
			<div class="flex flex-col items-center justify-center py-20 text-base-content/50">
				<remote.icon class="mb-4 h-12 w-12 opacity-20" />
				<p class="text-lg font-medium">{search ? 'No matches' : 'No tracks yet'}</p>
				<p class="mt-1 text-sm">
					{search ? 'Try another search term.' : `Add music to ${remote.label} to see tracks here.`}
				</p>
			</div>
		{/snippet}
		{#snippet children(tracks)}
			{#if playableTracks.length > 0}
				<div class="flex items-center gap-2 mb-4">
					{#if loader.loading}
						<Button
							variant="primary"
							size="sm"
							onclick={() => loader.abort()}
							aria-label="Stop loading tracks"
						>
							<LoaderCircle class="h-3.5 w-3.5 animate-spin motion-keep" aria-hidden="true" />
							{loader.progressText ?? 'Loading tracks'}
						</Button>
					{:else}
						<Button
							variant="primary"
							size="sm"
							icon={Play}
							class="[&_svg]:fill-current"
							onclick={() => loader.playAll(tracks.items, tracks.total)}
							aria-label="Play all tracks"
						>
							Play All
						</Button>
						<Button
							variant="ghost"
							size="sm"
							icon={Shuffle}
							onclick={() => loader.shuffleAll(tracks.items, tracks.total)}
							aria-label="Shuffle all tracks"
						>
							Shuffle
						</Button>
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
						{#each tracks.items as track, i (track.id)}
							{@const playing = isTrackPlaying(track)}
							{@const playable = remote.playKey(track) !== null}
							<tr
								class="group transition-colors focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary {playing
									? 'bg-accent/10'
									: playable
										? 'hover:bg-base-200/50 cursor-pointer'
										: 'opacity-50'}"
								onclick={() => playable && playTrack(track)}
								onkeydown={(e) =>
									playable &&
									e.target === e.currentTarget &&
									(e.key === 'Enter' || e.key === ' ') &&
									(e.preventDefault(), playTrack(track))}
								tabindex={playable ? 0 : -1}
								role={playable ? 'button' : undefined}
								aria-label={playable ? `Play ${track.title}` : undefined}
							>
								<td class="text-center">
									<div class="flex items-center justify-center w-8 h-8">
										{#if playing}
											<NowPlayingIndicator />
										{:else if playable}
											<span class="text-base-content/40 text-sm tabular-nums group-hover:hidden"
												>{i + 1}</span
											>
											<Play
												class="h-3.5 w-3.5 fill-current text-primary hidden group-hover:block"
											/>
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
									<span class="text-sm text-base-content/50 truncate block">{track.album_name}</span
									>
								</td>
								<td class="text-right hidden sm:table-cell">
									<span
										class="text-sm text-base-content/40 tabular-nums {playing
											? 'text-accent/60'
											: ''}"
									>
										{formatDurationSec(track.duration_secs ?? 0)}
									</span>
								</td>
								<td class="text-right hidden sm:table-cell">
									<div
										class="flex items-center justify-end gap-1 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100 transition-opacity"
									>
										<ContextMenu items={getTrackMenuItems(track)} position="end" size="xs" />
									</div>
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>

			<Pager page={currentPage} pages={totalPages} ongo={goToPage} />
		{/snippet}
	</QueryBoundary>
</div>
