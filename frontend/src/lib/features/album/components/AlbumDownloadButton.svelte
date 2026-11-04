<script lang="ts">
	import { v3 } from '$lib/api/v3/endpoint';
	import { Download } from 'lucide-svelte';
	import { downloadAlbumArchive } from '$lib/features/album/components/downloadActions';
	import { formatBytes } from '$lib/utils/formatting';
	import Button, { type ButtonSize, type ButtonVariant } from '$lib/ui/Button.svelte';

	interface Props {
		/** Library string id (preferred); when null the RG-MBID variant is used. */
		albumId: string | null;
		/** Release-group MBID fallback when no library id is known. */
		mbid: string | null;
		/** Total archive size for the caption; null/0 renders a bare `ZIP`. */
		totalSizeBytes: number | null;
		/** Local track count; 0 disables the button (nothing to archive). */
		trackCount: number;
		/** Server-computed permission bit; false renders nothing. */
		downloadAllowed: boolean;
		size?: ButtonSize;
		variant?: ButtonVariant;
	}

	let {
		albumId,
		mbid,
		totalSizeBytes,
		trackCount,
		downloadAllowed,
		size = 'md',
		variant = 'outline'
	}: Props = $props();

	const href = $derived(
		albumId
			? v3('/api/v3/download/local/album/{id}', { path: { id: albumId } })
			: mbid
				? v3('/api/v3/download/local/album/mbid/{mbid}', { path: { mbid } })
				: null
	);
	const caption = $derived(
		totalSizeBytes && totalSizeBytes > 0 ? `ZIP · ${formatBytes(totalSizeBytes)}` : 'ZIP'
	);

	let pending = $state(false);

	async function handleDownload() {
		if (!href || pending) return;
		pending = true;
		try {
			await downloadAlbumArchive(href);
		} finally {
			pending = false;
		}
	}
</script>

{#if href && downloadAllowed}
	<Button
		{variant}
		{size}
		icon={Download}
		loading={pending}
		disabled={trackCount === 0}
		title={trackCount === 0 ? 'No local files to archive' : caption}
		onclick={() => void handleDownload()}
	>
		{#if pending}
			Preparing…
		{:else}
			Download<span class="sr-only"> album, {caption}</span>
		{/if}
	</Button>
{/if}
