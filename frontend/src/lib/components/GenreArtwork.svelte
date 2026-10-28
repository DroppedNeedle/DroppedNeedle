<script lang="ts">
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import type { GenreArtwork as GenreArtworkModel } from '$lib/queries/discover/types';

	interface Props {
		artwork?: GenreArtworkModel;
		gradientClass: string;
	}

	let { artwork = undefined, gradientClass }: Props = $props();
	let albums = $derived(artwork?.albums.slice(0, 4) ?? []);
	let layoutClass = $derived(
		albums.length === 1
			? 'grid-cols-1 grid-rows-1'
			: albums.length === 2
				? 'grid-cols-2 grid-rows-1'
				: 'grid-cols-2 grid-rows-2'
	);
	// A lone cover fills the tile; two to four share it, so a small rendition is enough.
	let requestSize = $derived<250 | 500>(albums.length === 1 ? 500 : 250);
</script>

<div class="absolute inset-0 bg-linear-to-br {gradientClass}" data-testid="genre-gradient"></div>

{#if albums.length > 0}
	<div
		class="absolute inset-0 grid {layoutClass}"
		data-testid="genre-artwork-{albums.length}"
		data-version={artwork?.version}
	>
		{#each albums as album, index (album.album_id)}
			<div
				class:row-span-2={albums.length === 3 && index === 0}
				class="min-h-0 min-w-0"
				data-testid="genre-artwork-cell"
			>
				<AlbumImage
					albumId={album.album_id}
					coverVersion={album.cover_version}
					{requestSize}
					alt=""
					size="full"
					rounded="none"
					showPlaceholder={false}
					testId="genre-artwork-image"
					className="h-full w-full"
				/>
			</div>
		{/each}
	</div>
{/if}
