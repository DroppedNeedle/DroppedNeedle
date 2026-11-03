<script lang="ts">
	import { Library } from 'lucide-svelte';
	import HorizontalCarousel from '$lib/components/HorizontalCarousel.svelte';
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import LibraryAlbumCard from './LibraryAlbumCard.svelte';
	import { getLibraryArtistAlbumsQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import { getArtistOwnedReleasesQuery } from '$lib/queries/artist/ArtistQueries.svelte';
	import { fileOwnedAlbums, type FiledAlbum } from './artistLibrarySections';

	interface Props {
		/** The library artist's id. */
		libraryArtistId: string;
		/** The artist's MusicBrainz id, for the release types. */
		artistMbid: string;
	}

	let { libraryArtistId, artistMbid }: Props = $props();

	const albumsQuery = getLibraryArtistAlbumsQuery(() => libraryArtistId);
	const ownedQuery = getArtistOwnedReleasesQuery(() => artistMbid);

	// Without the types (MusicBrainz down) the albums still show, filed by
	// their compilation flag.
	const sections = $derived(
		fileOwnedAlbums(albumsQuery.data?.items ?? [], ownedQuery.data?.items ?? [])
	);
	const loading = $derived(albumsQuery.isLoading || ownedQuery.isLoading);
	const groups = $derived(
		[
			{ id: 'albums', title: 'Albums', items: sections.albums },
			{ id: 'eps-singles', title: 'EPs & Singles', items: sections.epsAndSingles },
			{ id: 'other', title: 'Compilations, Live & more', items: sections.other }
		].filter((group) => group.items.length > 0)
	);
</script>

{#snippet row(items: FiledAlbum[])}
	<HorizontalCarousel>
		{#each items as filed (filed.album.id)}
			<div class="w-36 shrink-0">
				<LibraryAlbumCard album={filed.album} kind={filed.label ?? undefined} />
			</div>
		{/each}
	</HorizontalCarousel>
{/snippet}

{#if loading}
	<div>
		<h3 class="text-lg font-semibold mb-3 flex items-center gap-2">
			<Library class="w-5 h-5 text-primary" />
			In your library
		</h3>
		<CarouselSkeleton count={6} cardWidth="w-36" />
	</div>
{:else if groups.length > 0}
	<section
		id="section-library-albums"
		class="space-y-4 scroll-mt-24"
		aria-labelledby="library-albums-title"
	>
		<h3 id="library-albums-title" class="text-lg font-semibold flex items-center gap-2">
			<Library class="w-5 h-5 text-primary" />
			In your library
		</h3>
		{#each groups as group (group.id)}
			<div>
				<h4 class="mb-2 text-sm font-semibold text-base-content/70">
					{group.title}
					<span class="ml-1 font-normal text-base-content/45">{group.items.length}</span>
				</h4>
				{@render row(group.items)}
			</div>
		{/each}
	</section>
{/if}
