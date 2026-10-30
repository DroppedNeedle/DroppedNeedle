<!--
	Every retag pair that names one album, read page by page from the
	server. Admin only.
-->
<script lang="ts">
	import { getAlbumRetagPairsQuery } from '$lib/queries/library/LibraryRetagPairQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import RetagPairCard from './RetagPairCard.svelte';

	interface Props {
		albumId: string;
		/** Classes for the box around each card. */
		class?: string;
	}

	let { albumId, class: className = '' }: Props = $props();

	const query = getAlbumRetagPairsQuery(
		() => albumId,
		() => authStore.isAdmin
	);
	const pairs = $derived((query.data?.pages ?? []).flatMap((page) => page.items));

	// An album has few pairs: read every page.
	$effect(() => {
		if (query.hasNextPage && !query.isFetchingNextPage && !query.isFetchNextPageError) {
			void query.fetchNextPage();
		}
	});
</script>

{#each pairs as pair (`${pair.old_album.id}/${pair.new_album.id}`)}
	<div class={className}><RetagPairCard {pair} currentAlbumId={albumId} /></div>
{/each}
