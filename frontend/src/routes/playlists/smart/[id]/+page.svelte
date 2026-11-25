<script lang="ts">
	import { Pencil } from 'lucide-svelte';
	import SmartPlaylistEditor from '$lib/features/playlists/components/SmartPlaylistEditor.svelte';
	import SmartPlaylistTracks from '$lib/features/playlists/components/SmartPlaylistTracks.svelte';
	import { getSmartPlaylistQuery } from '$lib/features/playlists/queries/SmartPlaylistQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type { PageProps } from './$types';

	let { params }: PageProps = $props();
	const definition = getSmartPlaylistQuery(() => params.id);
	const currentId = $derived(authStore.user?.id + '/' + params.id);
	let editingId = $state<string | null>(null);
</script>

<svelte:head><title>{definition.data?.name ?? 'Smart playlist'} - DroppedNeedle</title></svelte:head
>

<QueryBoundary query={definition} what="this smart playlist">
	{#snippet children(playlist)}
		<ListPageHeader
			title={playlist.name}
			fitTitle
			subtitle={'My rating is at least ' +
				playlist.min_rating +
				(playlist.min_rating === 1 ? ' star' : ' stars')}
			ticket="DN / CC 3.2"
			backHref={withBasePath('/playlists')}
			backLabel="Back to playlists"
		>
			{#snippet actions()}<Button
					variant="outline"
					icon={Pencil}
					onclick={() => (editingId = currentId)}>Edit rule</Button
				>{/snippet}
		</ListPageHeader>
		<div class="space-y-4 px-4 pb-10 sm:px-6 lg:px-8">
			{#key currentId + '/' + playlist.min_rating}
				<SmartPlaylistTracks id={playlist.id} />
			{/key}
		</div>
		{#if editingId === currentId}
			<SmartPlaylistEditor {playlist} onclose={() => (editingId = null)} />
		{/if}
	{/snippet}
</QueryBoundary>
