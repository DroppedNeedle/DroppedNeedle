<script lang="ts">
	import { resolve } from '$app/paths';
	import type { DiscoveryAlbum, Album } from '$lib/types';
	import HorizontalCarousel from './HorizontalCarousel.svelte';
	import CarouselSkeleton from './CarouselSkeleton.svelte';
	import AlbumCard from './AlbumCard.svelte';

	interface Props {
		albums: DiscoveryAlbum[];
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

	function toAlbum(da: DiscoveryAlbum): Album {
		return {
			title: da.title,
			artist: da.artist_name,
			year: da.year ?? null,
			musicbrainz_id: da.musicbrainz_id,
			in_library: da.in_library,
			requested: da.requested,
			cover_url: da.cover_url
		};
	}
</script>

<div class="mb-8">
	<h3 class="text-lg font-semibold mb-3">{title}</h3>

	{#if loading}
		<CarouselSkeleton count={6} cardWidth="w-36" />
	{:else if !configured}
		<div class="bg-base-200 rounded-lg p-6 text-center">
			<p class="text-base-content/70">Connect a music service to see recommendations</p>
			<a href={resolve('/profile#scrobbling')} class="btn btn-primary btn-sm mt-3">Configure</a>
		</div>
	{:else if albums.length === 0}
		<div class="bg-base-200 rounded-lg p-6 text-center">
			<p class="text-base-content/70">{emptyMessage}</p>
		</div>
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
