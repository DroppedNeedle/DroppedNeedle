<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { editionSheet } from '$lib/features/library/components/edition/editionSheetState.svelte';
	import {
		cacheCanonicalLibraryAlbumDetail,
		getLibraryAlbumDetailQuery,
		getLibraryAlbumStatusQuery
	} from '$lib/features/library/queries/LibraryQueries.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import LocalAlbumPage from '$lib/features/album/components/LocalAlbumPage.svelte';
	import ProviderAlbumPage from '$lib/features/album/components/ProviderAlbumPage.svelte';
	import AlbumHeroSkeleton from '$lib/features/album/components/AlbumHeroSkeleton.svelte';

	interface Props {
		data: { albumId: string };
	}

	let { data }: Props = $props();
	// The status names the local album the asked id is, or the one holding it
	// when the id is a release group, whose page is the matched one. The id is
	// read as a local album only when the status names it, or cannot be read.
	const statusQuery = getLibraryAlbumStatusQuery(() => data.albumId);
	const localById = $derived(
		statusQuery.data ? statusQuery.data.album_id === data.albumId : statusQuery.isError
	);
	const statusSettled = $derived(statusQuery.data !== undefined || statusQuery.isError);
	const localQuery = getLibraryAlbumDetailQuery(() => (localById ? data.albumId : ''));
	const localAlbum = $derived(localQuery.data);
	// A guess its files contradict owns no group: the album is its own page.
	const providerAlbumId = $derived(
		localAlbum && !localAlbum.guess_contradicted ? (localAlbum.release_group_mbid ?? null) : null
	);
	const matchedElsewhere = $derived(providerAlbumId !== null && providerAlbumId !== data.albumId);
	// An edition chosen in the sheet can match a local album to a release
	// group; the page follows it there once the sheet is closed.
	const shouldRedirect = $derived(matchedElsewhere && !editionSheet.open);

	$effect(() => {
		if (localAlbum && shouldRedirect) {
			void cacheCanonicalLibraryAlbumDetail(localAlbum);
			// The query (`?edition=choose`) goes along to the album's page.
			void goto(`${albumHref(providerAlbumId ?? data.albumId)}${page.url.search}`, {
				replaceState: true
			});
		}
	});
</script>

{#if !statusSettled || localQuery.isLoading || shouldRedirect}
	<AlbumHeroSkeleton tracks />
{:else if localAlbum && (!providerAlbumId || matchedElsewhere)}
	<LocalAlbumPage albumId={localAlbum.id} />
{:else}
	<ProviderAlbumPage {data} {localAlbum} />
{/if}
