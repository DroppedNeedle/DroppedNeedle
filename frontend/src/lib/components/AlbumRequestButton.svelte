<script lang="ts">
	import { Download } from 'lucide-svelte';
	import { requestAlbum } from '$lib/queries/downloads/DownloadMutations.svelte';

	interface Props {
		mbid: string;
		artistName: string;
		albumName: string;
		year?: number | null;
		artistMbid?: string;
		size?: 'sm' | 'md';
	}

	let { mbid, artistName, albumName, year, artistMbid, size = 'sm' }: Props = $props();

	let requesting = $state(false);

	const albumRequest = requestAlbum();

	async function handleRequest(e: MouseEvent) {
		e.stopPropagation();
		e.preventDefault();
		if (requesting) return;
		requesting = true;
		try {
			await albumRequest
				.mutateAsync({
					release_group_mbid: mbid,
					artist_name: artistName || undefined,
					album_title: albumName,
					year: year ?? undefined,
					artist_mbid: artistMbid
				})
				.catch(() => null);
		} finally {
			requesting = false;
		}
	}
</script>

<button
	type="button"
	class="btn btn-circle btn-sm {size === 'sm'
		? 'min-h-[36px] min-w-[36px]'
		: 'min-h-[44px] min-w-[44px]'} border-none bg-accent text-accent-content shadow-sm active:scale-[0.95]"
	title={requesting ? 'Requesting...' : 'Request album'}
	aria-label="Request {albumName}"
	disabled={requesting}
	onclick={handleRequest}
>
	{#if requesting}
		<span class="loading loading-spinner loading-xs"></span>
	{:else}
		<Download class={size === 'sm' ? 'h-4 w-4' : 'h-5 w-5'} />
	{/if}
</button>
