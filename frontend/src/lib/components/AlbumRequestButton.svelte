<script lang="ts">
	import { Download } from 'lucide-svelte';
	import { requestAlbum } from '$lib/queries/downloads/DownloadMutations.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

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

<IconButton
	variant="accent"
	{size}
	icon={Download}
	label="Request {albumName}"
	class={size === 'sm' ? 'min-h-[36px] min-w-[36px]' : 'min-h-[44px] min-w-[44px]'}
	loading={requesting}
	onclick={handleRequest}
/>
