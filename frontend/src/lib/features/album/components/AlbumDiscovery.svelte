<script lang="ts">
	import type { MoreByArtistResponse, SimilarAlbumsResponse } from '$lib/types';
	import DiscoveryAlbumCarousel from '$lib/features/album/components/DiscoveryAlbumCarousel.svelte';

	interface Props {
		moreByArtist: MoreByArtistResponse | null;
		similarAlbums: SimilarAlbumsResponse | null;
		loadingDiscovery: boolean;
		artistName: string;
	}

	let { moreByArtist, similarAlbums, loadingDiscovery, artistName }: Props = $props();
</script>

<div class="mt-8">
	<DiscoveryAlbumCarousel
		albums={moreByArtist?.albums || []}
		loading={loadingDiscovery}
		configured={true}
		title="More by {moreByArtist?.artist_name || artistName}"
		emptyMessage="No other albums found"
	/>
</div>

<DiscoveryAlbumCarousel
	albums={similarAlbums?.albums || []}
	loading={loadingDiscovery}
	configured={similarAlbums?.configured ?? true}
	title="You Might Also Like"
	emptyMessage="No similar albums found"
/>
