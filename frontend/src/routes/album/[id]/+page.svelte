<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { editionSheet } from '$lib/components/library/edition/editionSheetState.svelte';
	import {
		cacheCanonicalLibraryAlbumDetail,
		getLibraryAlbumDetailQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import LocalAlbumPage from './LocalAlbumPage.svelte';
	import ProviderAlbumPage from './ProviderAlbumPage.svelte';

	interface Props {
		data: { albumId: string };
	}

	let { data }: Props = $props();
	const localQuery = getLibraryAlbumDetailQuery(() => data.albumId);
	const localAlbum = $derived(localQuery.data);
	const providerAlbumId = $derived(localAlbum?.musicbrainz_release_group_id ?? null);
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

{#if localQuery.isLoading || shouldRedirect}
	<div class="w-full max-w-7xl mx-auto px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
		<div class="grid gap-6 lg:grid-cols-[20rem_1fr]">
			<div class="skeleton aspect-square w-full rounded-box"></div>
			<div class="space-y-4 self-end">
				<div class="skeleton h-12 w-3/4"></div>
				<div class="skeleton h-6 w-1/2"></div>
				<div class="skeleton h-12 w-48"></div>
			</div>
		</div>
	</div>
{:else if localAlbum && (!providerAlbumId || matchedElsewhere)}
	<LocalAlbumPage albumId={localAlbum.id} />
{:else}
	<ProviderAlbumPage {data} {localAlbum} />
{/if}
