<script lang="ts">
	import AlbumImage from './AlbumImage.svelte';
	import ArtistImage from './ArtistImage.svelte';
	import ArtistCardDownloadButton from './ArtistCardDownloadButton.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';

	interface Props {
		name: string;
		imageUrl?: string | null;
		mbid?: string | null;
		href?: string;
		albumCount?: number;
	}

	let { name, imageUrl = null, mbid = null, href, albumCount }: Props = $props();
</script>

<Card
	variant="raised"
	padding="none"
	{href}
	class="group relative w-full shrink-0"
	aria-label={href ? `Open ${name}` : undefined}
>
	{#if href && mbid}
		<ArtistCardDownloadButton artistName={name} artistMbid={mbid} />
	{/if}
	<figure class="aspect-square p-3">
		{#if mbid}
			<ArtistImage
				{mbid}
				alt={name}
				size="full"
				requestSize={250}
				remoteUrl={imageUrl}
				className="w-full h-full"
			/>
		{:else if imageUrl}
			<!-- A server's own artist picture has no MusicBrainz id, so it loads by URL. -->
			<AlbumImage
				customUrl={imageUrl}
				alt={name}
				size="full"
				requestSize={250}
				rounded="full"
				className="w-full h-full"
			/>
		{:else}
			<div class="relative h-full w-full overflow-hidden rounded-full">
				<CoverPlaceholder kind="artist" {name} />
			</div>
		{/if}
	</figure>
	<div class="flex flex-col items-center gap-1 p-2 pt-0 text-center">
		<CardTitle class="text-xs line-clamp-1 min-h-[1.25rem]">{name}</CardTitle>
		{#if albumCount !== undefined}
			<p class="text-xs text-base-content/50">{albumCount} album{albumCount !== 1 ? 's' : ''}</p>
		{/if}
	</div>
</Card>
