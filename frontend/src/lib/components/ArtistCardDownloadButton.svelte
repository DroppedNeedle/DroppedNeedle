<script lang="ts">
	import { Download } from 'lucide-svelte';
	import {
		discographyDownloadStore,
		type DiscographyRelease
	} from '$lib/stores/discographyDownload.svelte';
	import { api } from '$lib/api/client';
	import { toArtistReleases } from '$lib/queries/catalog/catalogAdapters';
	import { CATALOG_ENDPOINTS } from '$lib/queries/catalog/endpoints';
	import { toastStore } from '$lib/stores/toast';

	interface Props {
		artistName: string;
		artistMbid: string;
	}

	let { artistName, artistMbid }: Props = $props();

	let loading = $state(false);

	async function handleClick(e: MouseEvent) {
		e.preventDefault();
		e.stopPropagation();
		if (loading) return;
		loading = true;

		try {
			const data = toArtistReleases(
				await api.global.v3.GET(CATALOG_ENDPOINTS.artistReleases(artistMbid, 0, 200))
			);

			const allReleases: DiscographyRelease[] = [
				...data.albums.map((r) => ({ ...r, type: r.type ?? 'Album' })),
				...data.eps.map((r) => ({ ...r, type: r.type ?? 'EP' })),
				...data.singles.map((r) => ({ ...r, type: r.type ?? 'Single' }))
			];

			if (allReleases.length === 0) {
				toastStore.show({ message: 'No releases found for this artist', type: 'info' });
				return;
			}

			discographyDownloadStore.show(artistName, artistMbid, allReleases);
		} catch {
			toastStore.show({ message: 'Failed to load releases', type: 'error' });
		} finally {
			loading = false;
		}
	}
</script>

<button
	class="absolute top-1.5 right-1.5 z-10 btn btn-circle btn-xs bg-base-100/80 hover:bg-base-100 border-none shadow-md opacity-0 group-hover:opacity-100 transition-opacity duration-200"
	onclick={handleClick}
	title="Download discography"
	aria-label="Download discography for {artistName}"
>
	{#if loading}
		<span class="loading loading-spinner loading-xs"></span>
	{:else}
		<Download class="h-3 w-3" />
	{/if}
</button>
