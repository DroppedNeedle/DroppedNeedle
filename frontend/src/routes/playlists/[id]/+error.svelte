<script lang="ts">
	import { Music } from 'lucide-svelte';
	import { page } from '$app/state';
	import PlaylistNotice from '$lib/features/playlists/components/PlaylistNotice.svelte';

	const status = $derived(
		page.error && 'status' in page.error && page.error.status ? ` (${page.error.status})` : ''
	);
</script>

<svelte:head>
	<title>Playlist Error - DroppedNeedle</title>
</svelte:head>

<PlaylistNotice
	icon={Music}
	title="Couldn't load this playlist"
	message="{page.error?.message ?? 'Please try again'}{status}"
	onretry={() => location.reload()}
/>
