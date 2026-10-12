<script lang="ts">
	import { Search, ShoppingBag } from 'lucide-svelte';
	import { outsideLink } from '$lib/components/outsideLink';
	import { getArtistPurchaseOptionsQuery } from '$lib/queries/albums/GetItQueries.svelte';

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
<section aria-labelledby="artist-buy-heading" class="rounded-2xl bg-base-200/40 px-3 py-2">
	<div class="flex flex-wrap items-center gap-2">
		<div class="flex items-center gap-1.5">
			<ShoppingBag class="h-4 w-4 text-primary" aria-hidden="true" />
			<h2 id="artist-buy-heading" class="text-sm font-semibold">Support the artist</h2>
		</div>

		{#if optionsQuery.isLoading}
			<div class="skeleton h-6 w-28 rounded-full"></div>
		{:else if options}
			{#each links as link (link.url)}
				<a
					href={link.url}
					target="_blank"
					rel="noopener noreferrer"
					class="btn btn-xs rounded-full border-base-content/15 bg-base-200 hover:border-primary hover:bg-base-100"
				>
					{link.label}
				</a>
			{/each}
			{#if options.bandcamp_search_url}
				<a
					href={options.bandcamp_search_url}
					target="_blank"
					rel="noopener noreferrer"
					class="btn btn-xs rounded-full border-dashed border-base-content/25 bg-transparent hover:border-primary"
				>
					<Search class="h-3 w-3" aria-hidden="true" />
					{links.length ? 'More on Bandcamp' : 'Search Bandcamp'}
				</a>
			{/if}
		{/if}
	</div>
</section>
