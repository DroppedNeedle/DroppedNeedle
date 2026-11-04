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
	// The status names the local album that holds the asked id. When that is
	// another album, the id is a release group the library holds: its page is
	// the matched one, and the id is never read as a local album.
	const statusQuery = getLibraryAlbumStatusQuery(() => data.albumId);
	const heldByAnotherAlbum = $derived(
		statusQuery.data !== undefined && statusQuery.data.album_id !== data.albumId
	);
	const statusSettled = $derived(statusQuery.data !== undefined || statusQuery.isError);
	const localQuery = getLibraryAlbumDetailQuery(() =>
		statusSettled && !heldByAnotherAlbum ? data.albumId : ''
	);
	const localAlbum = $derived(localQuery.data);
	const providerAlbumId = $derived(localAlbum?.release_group_mbid ?? null);
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
