<script lang="ts">
	import { goto } from '$app/navigation';
	import type { MusicSource } from '$lib/stores/musicSource';
	import {
		cacheCanonicalLibraryArtistDetail,
		getLibraryArtistDetailQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import { artistHref } from '$lib/utils/entityRoutes';
	import LocalArtistPage from './LocalArtistPage.svelte';
	import ProviderArtistPage from './ProviderArtistPage.svelte';

	interface Props {
		data: { artistId: string; primarySource: MusicSource };
	}

	let { data }: Props = $props();
	const localQuery = getLibraryArtistDetailQuery(() => data.artistId);
	const localArtist = $derived(localQuery.data);
	const providerArtistId = $derived(localArtist?.musicbrainz_artist_id ?? null);
	const shouldRedirect = $derived(providerArtistId !== null && providerArtistId !== data.artistId);

	$effect(() => {
		if (localArtist && shouldRedirect) {
			void cacheCanonicalLibraryArtistDetail(localArtist);
			void goto(artistHref(providerArtistId ?? data.artistId), { replaceState: true });
		}
	});
</script>

<!--
	Gate the skeleton on `!isFetched` (monotonic once the library lookup first
	resolves), NOT on `isLoading` alone. ProviderArtistPage re-observes this same
	library query for its MusicBrainz-down fallback; for an unowned artist that
	404s, its mount kicks off a refetch that flips `isLoading`/`status:pending`
	back on. Gating on `isLoading` alone would swap back to the skeleton, unmount
	the provider page, cancel that refetch, and remount - an infinite loop that
	hammers /api/v1/library/artists/{id} (see GH-339 / GH-341).
-->
{#if (localQuery.isLoading && !localQuery.isFetched) || shouldRedirect}
	<div class="w-full max-w-7xl mx-auto px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
		<div class="flex items-end gap-6">
			<div class="skeleton h-48 w-48 rounded-full"></div>
			<div class="mb-4 w-full max-w-xl space-y-4">
				<div class="skeleton h-12 w-3/4"></div>
				<div class="skeleton h-6 w-1/2"></div>
			</div>
		</div>
	</div>
{:else if localArtist && !providerArtistId}
	<LocalArtistPage artistId={localArtist.id} />
{:else}
	<ProviderArtistPage {data} />
{/if}
