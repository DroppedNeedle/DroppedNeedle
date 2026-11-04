<script lang="ts">
	import { EAGER_IMAGE_COUNT } from '$lib/media/lazyImage';
	import type { Album } from '$lib/features/album/queries/types';
	import type { AlbumRef } from '$lib/features/discover/queries/types';
	import { withBasePath } from '$lib/utils/basePath';
	import HorizontalCarousel from '$lib/ui/HorizontalCarousel.svelte';
	import CarouselSkeleton from '$lib/ui/CarouselSkeleton.svelte';
	import AlbumCard from './AlbumCard.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		albums: AlbumRef[];
		loading?: boolean;
		configured?: boolean;
		title: string;
		emptyMessage?: string;
	}

	let {
		albums,
		loading = false,
		configured = true,
		title,
		emptyMessage = 'No albums found'
	}: Props = $props();

	function toAlbum(da: AlbumRef): Album {
		return {
			title: da.title,
			artist: da.artist_name ?? null,
			year: da.year ?? null,
			musicbrainz_id: da.musicbrainz_id ?? '',
			in_library: da.in_library,
			requested: da.requested,
			cover_url: da.cover_url ?? null
		};
	}
</script>

<div class="mb-8">
	<h3 class="mb-3 text-xl font-bold">{title}</h3>

	{#if loading}
		<CarouselSkeleton count={6} cardWidth="w-36" />
	{:else if !configured}
		<Card>
			<div class="text-center">
				<p class="text-muted">Connect a music service to see recommendations</p>
				<Button variant="primary" size="sm" href={withBasePath('/profile#scrobbling')} class="mt-3">
					Configure
				</Button>
			</div>
		</Card>
	{:else if albums.length === 0}
		<Card>
			<p class="text-center text-muted">{emptyMessage}</p>
		</Card>
	{:else}
		<HorizontalCarousel>
			{#each albums as album, index (album.musicbrainz_id)}
				<div class="w-36 shrink-0">
					<AlbumCard album={toAlbum(album)} lazy={index >= EAGER_IMAGE_COUNT} />
				</div>
			{/each}
		</HorizontalCarousel>
	{/if}
</div>
