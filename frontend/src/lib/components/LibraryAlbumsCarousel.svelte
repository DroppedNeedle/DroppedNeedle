<script lang="ts">
	import { EAGER_IMAGE_COUNT } from '$lib/utils/lazyImage';
	import type { ReleaseGroup, Album } from '$lib/types';
	import HorizontalCarousel from './HorizontalCarousel.svelte';
	import CarouselSkeleton from './CarouselSkeleton.svelte';
	import AlbumCard from './AlbumCard.svelte';
	import { libraryStore } from '$lib/stores/library';
	import { Library } from 'lucide-svelte';

	interface Props {
		releases: ReleaseGroup[];
		artistName: string;
		loading?: boolean;
	}

	let { releases, artistName, loading = false }: Props = $props();

	let libraryReleases = $derived(
		releases
			.filter((r) => libraryStore.isInLibrary(r.id) || r.in_library)
			.sort((a, b) => {
				const ya = a.year ?? 0;
				const yb = b.year ?? 0;
				return yb - ya;
			})
	);

	function toAlbum(rg: ReleaseGroup): Album {
		return {
			title: rg.title,
			artist: artistName,
			year: rg.year ?? null,
			musicbrainz_id: rg.id,
			in_library: libraryStore.isInLibrary(rg.id) || rg.in_library,
			requested: rg.requested,
			cover_url: null,
			type_info: rg.type
		};
	}
</script>

{#if loading}
	<div>
		<h3 class="text-lg font-semibold mb-3 flex items-center gap-2">
			<Library class="w-5 h-5 text-primary" />
			In your library
		</h3>
		<CarouselSkeleton count={6} cardWidth="w-36" />
	</div>
{:else if libraryReleases.length > 0}
	<div>
		<h3 class="text-lg font-semibold mb-3 flex items-center gap-2">
			<Library class="w-5 h-5 text-primary" />
			In your library
		</h3>
		<HorizontalCarousel>
			{#each libraryReleases as release, index (release.id)}
				<div class="w-36 shrink-0">
					<AlbumCard album={toAlbum(release)} lazy={index >= EAGER_IMAGE_COUNT} />
				</div>
			{/each}
		</HorizontalCarousel>
	</div>
{/if}
