<script lang="ts">
	import { goto } from '$app/navigation';
	import {
		cacheCanonicalLibraryAlbumDetailV3,
		getLibraryAlbumDetailV3Query
	} from '$lib/queries/library/LibraryV3Queries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import LocalAlbumPage from './LocalAlbumPage.svelte';
	import ProviderAlbumPage from './ProviderAlbumPage.svelte';

	interface Props {
		data: { albumId: string };
	}

	let { data }: Props = $props();
	const localQuery = getLibraryAlbumDetailV3Query(() => data.albumId);
	const localAlbum = $derived(localQuery.data);
	const providerAlbumId = $derived(localAlbum?.release_group_mbid ?? null);
	const shouldRedirect = $derived(providerAlbumId !== null && providerAlbumId !== data.albumId);

	$effect(() => {
		if (localAlbum && shouldRedirect) {
			void cacheCanonicalLibraryAlbumDetailV3(authStore.user?.id, localAlbum);
			void goto(albumHref(providerAlbumId ?? data.albumId), { replaceState: true });
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
{:else if localAlbum && !providerAlbumId}
	<LocalAlbumPage albumId={localAlbum.id} />
{:else}
	<ProviderAlbumPage {data} {localAlbum} />
{/if}
