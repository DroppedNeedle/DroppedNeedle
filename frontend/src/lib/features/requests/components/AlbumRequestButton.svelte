<!--
	Requests an album. A round icon by default; `labelled` is the small
	"Request" button that sits under a shelf tile.
-->
<script lang="ts">
	import { Download } from 'lucide-svelte';
	import { createRequestAlbumMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		mbid: string;
		artistName: string;
		albumName: string;
		year?: number | null;
		artistMbid?: string;
		size?: 'sm' | 'md';
		/** A small button with a visible "Request" label, as wide as its tile. */
		labelled?: boolean;
	}

	let {
		mbid,
		artistName,
		albumName,
		year,
		artistMbid,
		size = 'sm',
		labelled = false
	}: Props = $props();

	let requesting = $state(false);

	const albumRequest = createRequestAlbumMutation();

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

{#if labelled}
	<Button
		variant="primary"
		size="xs"
		icon={Download}
		class="w-full"
		aria-label="Request {albumName}"
		title="Request {albumName}"
		loading={requesting}
		onclick={handleRequest}
	>
		Request
	</Button>
{:else}
	<IconButton
		variant="primary"
		{size}
		icon={Download}
		label="Request {albumName}"
		class={size === 'sm' ? 'min-h-[36px] min-w-[36px]' : 'min-h-[44px] min-w-[44px]'}
		loading={requesting}
		onclick={handleRequest}
	/>
{/if}
