<script lang="ts">
	import SourcePlaylistList from '$lib/components/SourcePlaylistList.svelte';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
	const remote = $derived(data.remote);
</script>

<SourcePlaylistList
	source={remote.source}
	sourceLabel={remote.label}
	backFallback="/library/{remote.source}"
	playlistBaseHref="/library/{remote.source}/playlists"
>
	{#snippet icon()}
		<remote.icon class="h-6 w-6 {remote.tone}" />
	{/snippet}
</SourcePlaylistList>
