<script lang="ts">
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import AlbumImage from './AlbumImage.svelte';

	interface Props {
		coverUrls?: string[];
		customCoverUrl?: string | null;
		size?: string;
		rounded?: string;
	}

	let {
		coverUrls = [],
		customCoverUrl = null,
		size = 'w-32 h-32',
		rounded = 'rounded-box'
	}: Props = $props();

	let urls = $derived(coverUrls.slice(0, 4));
</script>

{#snippet emptyCell()}
	<div class="relative w-full h-full overflow-hidden">
		<CoverPlaceholder />
	</div>
{/snippet}

{#snippet cover(url: string, alt: string)}
	<AlbumImage customUrl={url} {alt} size="full" rounded="none" className="w-full h-full" />
{/snippet}

<div class="overflow-hidden {rounded} {size}">
	{#if customCoverUrl}
		{@render cover(customCoverUrl, 'Playlist cover')}
	{:else if urls.length >= 2}
		<div class="grid grid-cols-2 w-full h-full gap-0" class:grid-rows-2={urls.length >= 3}>
			{#each urls as url, i (`${url}-${i}`)}
				{@render cover(url, '')}
			{/each}
			{#if urls.length === 3}
				{@render emptyCell()}
			{/if}
		</div>
	{:else if urls.length === 1}
		{@render cover(urls[0], 'Playlist cover')}
	{:else}
		{@render emptyCell()}
	{/if}
</div>
