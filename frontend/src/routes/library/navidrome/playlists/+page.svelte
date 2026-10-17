<script lang="ts">
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import SourcePlaylistList from '$lib/components/SourcePlaylistList.svelte';
</script>

<SourcePlaylistList
	source="navidrome"
	sourceLabel="Navidrome"
	backFallback="/library/navidrome"
	playlistBaseHref="/library/navidrome/playlists"
>
	{#snippet icon()}
		<span class="text-navidrome">
			<NavidromeIcon class="h-6 w-6" />
		</span>
	{/snippet}
</SourcePlaylistList>
