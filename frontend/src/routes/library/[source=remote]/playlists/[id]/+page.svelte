<script lang="ts">
	import { page } from '$app/state';
	import SourcePlaylistDetail from '$lib/components/SourcePlaylistDetail.svelte';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
	const remote = $derived(data.remote);
	const playlistId = $derived(page.params.id ?? '');
</script>

<SourcePlaylistDetail
	{playlistId}
	source={remote.source}
	backFallback="/library/{remote.source}/playlists"
>
	{#snippet icon()}
		<remote.icon class="h-6 w-6 {remote.tone}" />
	{/snippet}
</SourcePlaylistDetail>
