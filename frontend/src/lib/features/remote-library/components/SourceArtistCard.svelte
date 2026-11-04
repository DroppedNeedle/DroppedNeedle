<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import ArtistCardDownloadButton from '$lib/features/artist/components/ArtistCardDownloadButton.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
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

<MediaTile
	kind="artist"
	title={name}
	{href}
	label={href ? `Open ${name}` : undefined}
	class="w-full shrink-0"
>
	{#snippet art()}
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
	{/snippet}
	{#snippet meta()}
		{#if albumCount !== undefined}
			{albumCount} album{albumCount !== 1 ? 's' : ''}
		{/if}
	{/snippet}
	{#snippet overlay()}
		{#if href && mbid}
			<ArtistCardDownloadButton artistName={name} artistMbid={mbid} />
		{/if}
	{/snippet}
</MediaTile>
