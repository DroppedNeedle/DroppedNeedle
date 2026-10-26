<script lang="ts">
	import HorizontalCarousel from '$lib/components/HorizontalCarousel.svelte';
	import SourceAlbumCardCompact from '$lib/components/SourceAlbumCardCompact.svelte';
	import type { RemoteAlbum } from '$lib/queries/remotes/types';

	interface Props {
		albums: RemoteAlbum[];
		onopen: (album: RemoteAlbum) => void;
	}

	let { albums, onopen }: Props = $props();
</script>

<HorizontalCarousel>
	{#each albums as album (album.id)}
		<SourceAlbumCardCompact
			imageId={album.release_group_mbid ?? album.id}
			imageUrl={album.image_url}
			name={album.title}
			artistName={album.artist_name}
			onclick={() => onopen(album)}
		/>
	{/each}
</HorizontalCarousel>
