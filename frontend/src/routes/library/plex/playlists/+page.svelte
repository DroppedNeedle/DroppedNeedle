<script lang="ts">
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import SourcePlaylistList from '$lib/components/SourcePlaylistList.svelte';
</script>

<SourcePlaylistList
	source="plex"
	sourceLabel="Plex"
	backFallback="/library/plex"
	playlistBaseHref="/library/plex/playlists"
>
	{#snippet icon()}
		<PlexIcon class="h-6 w-6 text-plex" />
	{/snippet}
</SourcePlaylistList>
