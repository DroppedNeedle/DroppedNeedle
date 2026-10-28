<script lang="ts">
	import type { Album } from '$lib/types';
	import type { AlbumRef } from '$lib/queries/discover/types';
	import { withBasePath } from '$lib/utils/basePath';
	import HorizontalCarousel from './HorizontalCarousel.svelte';
	import CarouselSkeleton from './CarouselSkeleton.svelte';
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
	<h3 class="text-lg font-semibold mb-3">{title}</h3>

	{#if loading}
		<CarouselSkeleton count={6} cardWidth="w-36" />
	{:else if !configured}
		<Card>
			<div class="text-center">
				<p class="text-base-content/70">Connect a music service to see recommendations</p>
				<Button variant="primary" size="sm" href={withBasePath('/profile#scrobbling')} class="mt-3">
					Configure
				</Button>
			</div>
		</Card>
	{:else if albums.length === 0}
		<Card>
			<p class="text-center text-base-content/70">{emptyMessage}</p>
		</Card>
	{:else}
		<HorizontalCarousel>
			{#each albums as album (album.musicbrainz_id)}
				<div class="w-36 shrink-0">
					<AlbumCard album={toAlbum(album)} />
				</div>
			{/each}
		</HorizontalCarousel>
	{/if}
</div>
