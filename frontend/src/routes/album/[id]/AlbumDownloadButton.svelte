<script lang="ts">
	import { Download } from 'lucide-svelte';
	import { LocalDownloadsApi } from '$lib/queries/local/LocalDownloadsApi';
	import { downloadAlbumArchive } from '$lib/utils/downloadActions';
	import { formatBytes } from '$lib/utils/formatting';

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
		className?: string;
	}

	let {
		albumId,
		mbid,
		totalSizeBytes,
		trackCount,
		downloadAllowed,
		className = ''
	}: Props = $props();

	const href = $derived(
		albumId ? LocalDownloadsApi.album(albumId) : mbid ? LocalDownloadsApi.albumByMbid(mbid) : null
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
	<button
		type="button"
		class="btn btn-primary gap-2 {className}"
		disabled={pending || trackCount === 0}
		title={trackCount === 0 ? 'No local files to archive' : undefined}
		onclick={() => void handleDownload()}
	>
		{#if pending}
			<span class="loading loading-spinner loading-sm"></span>
		{:else}
			<Download class="h-4 w-4" />
		{/if}
		<span>{pending ? 'Preparing…' : 'Download album'}</span>
		<span class="text-xs font-normal opacity-70">{caption}</span>
	</button>
{/if}
