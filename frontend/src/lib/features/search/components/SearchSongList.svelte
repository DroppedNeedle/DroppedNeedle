<!-- The calm panel of song rows on the All tab and the Songs tab. -->
<script lang="ts">
	import {
		isLibrarySong,
		searchSongKey,
		type SearchSong
	} from '$lib/features/search/queries/SearchV3Adapters';
	import SearchRemoteSongRow from './SearchRemoteSongRow.svelte';
	import SearchSongRow from './SearchSongRow.svelte';

	interface Props {
		songs: SearchSong[];
	}

	let { songs }: Props = $props();
</script>

<div
	class="divide-y divide-line overflow-hidden rounded-box border border-line bg-base-200"
	role="group"
	aria-label="Song search results"
>
	{#each songs as song (searchSongKey(song))}
		{#if isLibrarySong(song)}
			<SearchSongRow {song} />
		{:else}
			<SearchRemoteSongRow {song} />
		{/if}
	{/each}
</div>
