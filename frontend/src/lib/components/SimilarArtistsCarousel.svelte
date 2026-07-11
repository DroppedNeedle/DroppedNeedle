<script lang="ts">
	import { resolve } from '$app/paths';
	import { Check } from 'lucide-svelte';
	import type { SimilarArtist } from '$lib/types';
	import HorizontalCarousel from './HorizontalCarousel.svelte';
	import CarouselSkeleton from './CarouselSkeleton.svelte';
	import ArtistImage from './ArtistImage.svelte';
	import { colors } from '$lib/colors';
	import { artistHref } from '$lib/utils/entityRoutes';

	interface Props {
		artists: SimilarArtist[];
		loading?: boolean;
		configured?: boolean;
	}

	let { artists, loading = false, configured = true }: Props = $props();
</script>

<div>
	<h3 class="text-lg font-semibold mb-3">Similar Artists</h3>

	{#if loading}
		<CarouselSkeleton count={8} cardWidth="w-32" rounded="full" showSubtitle={false} />
	{:else if !configured}
		<div class="bg-base-200 rounded-lg p-6 text-center">
			<p class="text-base-content/70">Connect a music service to see similar artists</p>
			<a href={resolve('/profile#scrobbling')} class="btn btn-primary btn-sm mt-3">Configure</a>
		</div>
	{:else if artists.length === 0}
		<div class="bg-base-200 rounded-lg p-6 text-center">
			<p class="text-base-content/70">No similar artists found</p>
		</div>
	{:else}
		<HorizontalCarousel>
			{#each artists as artist (artist.musicbrainz_id)}
				<a href={artistHref(artist.musicbrainz_id)} class="w-32 shrink-0 cursor-pointer">
					<div class="relative group">
						<div
							class="aspect-square rounded-full overflow-hidden transition-transform group-hover:scale-105"
						>
							<ArtistImage
								mbid={artist.musicbrainz_id}
								alt={artist.name}
								size="full"
								requestSize={250}
								className="w-full h-full object-cover"
							/>
						</div>
						{#if artist.in_library}
							<div
								class="absolute bottom-1 right-1 rounded-full p-1"
								style="background-color: {colors.accent};"
							>
								<Check class="w-3 h-3" color={colors.secondary} strokeWidth={3} />
							</div>
						{/if}
					</div>
					<p class="text-center text-sm mt-2 truncate font-medium">{artist.name}</p>
				</a>
			{/each}
		</HorizontalCarousel>
	{/if}
</div>
