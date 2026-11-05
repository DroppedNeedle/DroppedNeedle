<script lang="ts">
	import { Search, ShoppingBag } from 'lucide-svelte';
	import { outsideLink } from '$lib/ui/outsideLink';
	import { getArtistPurchaseOptionsQuery } from '$lib/features/album/queries/GetItQueries.svelte';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		artistMbid: string;
		artistName: string;
	}
	let { artistMbid, artistName }: Props = $props();

	const optionsQuery = getArtistPurchaseOptionsQuery(
		() => artistMbid,
		() => artistName
	);
	const options = $derived(optionsQuery.data);
	const links = $derived((options?.links ?? []).filter((link) => outsideLink(link.url)));
</script>

<!-- The artist's own storefronts, in the About card. Always offers a way to
     buy: the Bandcamp artist search is the floor when MusicBrainz knows no store. -->
<section class="flex flex-wrap items-center gap-2" aria-labelledby="artist-support-title">
	<h3
		id="artist-support-title"
		class="mr-1 flex items-center gap-1.5 font-mono text-xs font-semibold tracking-[0.14em] text-muted uppercase"
	>
		<ShoppingBag class="size-3.5" aria-hidden="true" />
		Support the artist
	</h3>

	{#if optionsQuery.isLoading}
		<div class="skeleton h-6 w-28 rounded-full"></div>
	{:else if options}
		{#each links as link (link.url)}
			<Button size="xs" pill href={link.url} target="_blank" rel="noopener noreferrer">
				{link.label}
			</Button>
		{/each}
		{#if options.bandcamp_search_url}
			<Button
				size="xs"
				pill
				icon={Search}
				href={options.bandcamp_search_url}
				target="_blank"
				rel="noopener noreferrer"
			>
				{links.length ? 'More on Bandcamp' : 'Search Bandcamp'}
			</Button>
		{/if}
	{/if}
</section>
