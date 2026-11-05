<script lang="ts">
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import GenrePillFilter from './GenrePillFilter.svelte';
	import type { QueueItem } from '$lib/features/player/playback/types';
	import { Layers } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import DiscoveryShelf from './DiscoveryShelf.svelte';
	import DiscoveryTrackTable from './DiscoveryTrackTable.svelte';

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

<DiscoveryShelf
	title="Browse by Genre"
	icon={Layers}
	sub={multiSelect ? 'Pick one genre or several.' : 'One genre at a time.'}
	loading={loading && songs.length === 0}
	empty={songs.length === 0}
	emptyMessage={selectedGenres.length > 0
		? `No songs found for ${selectedGenres.join(', ')}.`
		: 'Pick a genre to browse tracks.'}
	onplayall={() => playSongs()}
	playAllLabel="Play All"
	onshuffle={shuffleSongs}
>
	{#snippet filters()}
		<GenrePillFilter
			{genres}
			selected={multiSelect ? undefined : selectedGenres[0]}
			selectedMultiple={multiSelect ? selectedGenres : undefined}
			{loading}
			{multiSelect}
			onselect={handleSelect}
		/>
	{/snippet}
	<DiscoveryTrackTable tracks={songs} onplay={(i) => playSongs(i)} />
	{#if songs.length >= offset + PAGE_SIZE}
		<div class="flex justify-center py-2">
			<Button variant="outline" size="display-sm" {loading} onclick={loadMore}>Show more</Button>
		</div>
	{/if}
</DiscoveryShelf>
