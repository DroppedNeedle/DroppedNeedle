<script lang="ts">
	import { Search, ShoppingBag } from 'lucide-svelte';
	import { outsideLink } from '$lib/components/outsideLink';
	import { getArtistPurchaseOptionsQuery } from '$lib/queries/albums/GetItQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

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

<!-- The artist's own storefronts, one line. Always renders a way to buy: the
     Bandcamp artist search is the floor when MusicBrainz knows no store. -->
<Card as="section" padding="sm" aria-label="Support the artist">
	<div class="flex flex-wrap items-center gap-2">
		<div class="flex items-center gap-1.5">
			<ShoppingBag class="h-4 w-4 text-primary" aria-hidden="true" />
			<h2 class="text-sm font-semibold">Support the artist</h2>
		</div>

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
	</div>
</Card>
