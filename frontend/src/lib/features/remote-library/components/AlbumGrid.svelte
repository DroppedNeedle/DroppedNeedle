<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import { ArrowRight } from 'lucide-svelte';
	import { reveal } from '$lib/actions/reveal';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import type { RemoteAlbum } from '$lib/features/remote-library/queries/types';

	interface Props {
		albums: RemoteAlbum[];
		maxItems?: number;
		seeAllHref?: string;
		seeAllLabel?: string;
		onAlbumClick?: (album: RemoteAlbum) => void;
	}

	let {
		albums,
		maxItems = 12,
		seeAllHref,
		seeAllLabel = 'View all',
		onAlbumClick
	}: Props = $props();

	let visible = $derived(albums.slice(0, maxItems));
</script>

<div use:reveal={{ stagger: 40 }}>
	<div class="dn-list-grid hub-grid">
		{#each visible as album (album.id)}
			<MediaTile
				title={album.title}
				label="Open {album.title} by {album.artist_name}"
				onclick={() => onAlbumClick?.(album)}
			>
				{#snippet art()}
					<AlbumImage
						mbid={album.release_group_mbid ?? album.id}
						customUrl={album.image_url}
						alt={album.title}
						size="full"
						requestSize={250}
						rounded="none"
						className="h-full w-full"
					/>
				{/snippet}
				{#snippet meta()}{album.artist_name}{/snippet}
			</MediaTile>
		{/each}
	</div>

	{#if seeAllHref && albums.length > maxItems}
		<div class="mt-4 flex justify-center">
			<Button
				variant="caps"
				size="sm"
				icon={ArrowRight}
				iconPosition="end"
				href={withBasePath(seeAllHref)}
			>
				{seeAllLabel}
			</Button>
		</div>
	{/if}
</div>

<style>
	/* six across on a desktop, so twelve albums fill two rows */
	.hub-grid {
		grid-template-columns: repeat(auto-fill, minmax(min(11.5rem, calc(50% - 0.625rem)), 1fr));
	}
</style>
