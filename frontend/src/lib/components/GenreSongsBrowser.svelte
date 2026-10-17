<script lang="ts">
	import { playerStore } from '$lib/stores/player.svelte';
	import { formatDurationSec } from '$lib/utils/formatting';
	import GenrePillFilter from './GenrePillFilter.svelte';
	import type { QueueItem } from '$lib/player/types';
	import Button from '$lib/ui/Button.svelte';

	export type BrowseTrack = {
		id: string;
		title: string;
		artist_name: string;
		album_name: string;
		duration_seconds: number;
		image_url?: string | null;
	};

	type Props = {
		genres: string[];
		fetchSongs: (genres: string[], limit: number, offset: number) => Promise<BrowseTrack[]>;
		buildQueue: (tracks: BrowseTrack[]) => QueueItem[];
		multiSelect?: boolean;
	};

	let { genres, fetchSongs, buildQueue, multiSelect = false }: Props = $props();

	let selectedGenres = $state<string[]>([]);
	let songs = $state<BrowseTrack[]>([]);
	let loading = $state(false);
	let offset = $state(0);
	const PAGE_SIZE = 50;

	async function loadSongs(genreList: string[], append = false) {
		if (genreList.length === 0) return;
		if (!append) {
			offset = 0;
			songs = [];
		}
		loading = true;
		try {
			const result = await fetchSongs(genreList, PAGE_SIZE, offset);
			songs = append ? [...songs, ...result] : result;
		} finally {
			loading = false;
		}
	}

	function handleSelect(genre: string | undefined) {
		if (!genre) {
			selectedGenres = [];
			songs = [];
			return;
		}
		if (multiSelect) {
			const idx = selectedGenres.indexOf(genre);
			if (idx >= 0) {
				selectedGenres = selectedGenres.filter((g) => g !== genre);
			} else {
				selectedGenres = [...selectedGenres, genre];
			}
			if (selectedGenres.length > 0) loadSongs(selectedGenres);
			else songs = [];
		} else {
			selectedGenres = [genre];
			loadSongs([genre]);
		}
	}

	function loadMore() {
		if (selectedGenres.length === 0) return;
		offset += PAGE_SIZE;
		loadSongs(selectedGenres, true);
	}

	function playSongs(startIndex = 0) {
		if (songs.length === 0) return;
		const items = buildQueue(songs);
		playerStore.playQueue(items, startIndex);
	}

	function shuffleSongs() {
		if (songs.length === 0) return;
		const items = buildQueue(songs);
		playerStore.playQueue(items, 0, true);
	}
</script>

<div class="space-y-3">
	<GenrePillFilter
		{genres}
		selected={multiSelect ? undefined : selectedGenres[0]}
		selectedMultiple={multiSelect ? selectedGenres : undefined}
		{loading}
		{multiSelect}
		onselect={handleSelect}
	/>

	{#if loading && songs.length === 0}
		<div class="flex justify-center py-4">
			<span class="loading loading-spinner loading-md"></span>
		</div>
	{:else if selectedGenres.length > 0 && songs.length > 0}
		<div class="flex items-center gap-2 mb-2">
			<Button variant="primary" size="sm" onclick={() => playSongs()}>Play All</Button>
			<Button variant="ghost" size="sm" onclick={shuffleSongs}>Shuffle</Button>
		</div>
		<div class="max-h-[32rem] overflow-y-auto overflow-x-auto rounded-lg">
			<table class="table table-sm">
				<thead>
					<tr>
						<th>#</th>
						<th>Title</th>
						<th>Artist</th>
						<th>Album</th>
						<th class="text-right">Duration</th>
					</tr>
				</thead>
				<tbody>
					{#each songs as track, i (track.id + '-' + i)}
						<tr
							class="hover cursor-pointer focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary"
							onclick={() => playSongs(i)}
							onkeydown={(e) =>
								e.target === e.currentTarget &&
								(e.key === 'Enter' || e.key === ' ') &&
								(e.preventDefault(), playSongs(i))}
							tabindex="0"
							role="button"
							aria-label="Play {track.title}"
						>
							<td class="text-base-content/50">{i + 1}</td>
							<td class="font-medium">{track.title}</td>
							<td class="text-base-content/60">{track.artist_name}</td>
							<td class="text-base-content/60">{track.album_name}</td>
							<td class="text-right text-base-content/50"
								>{formatDurationSec(track.duration_seconds)}</td
							>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
		{#if songs.length >= offset + PAGE_SIZE}
			<div class="flex justify-center">
				<Button variant="ghost" size="sm" {loading} onclick={loadMore}>Show more</Button>
			</div>
		{/if}
	{:else if selectedGenres.length > 0}
		<p class="text-sm text-base-content/50">No songs found for {selectedGenres.join(', ')}.</p>
	{:else}
		<p class="text-sm text-base-content/50">Pick a genre to browse tracks.</p>
	{/if}
</div>
