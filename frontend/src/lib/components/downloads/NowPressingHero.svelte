<script lang="ts">
	import { X } from 'lucide-svelte';

	import { cancelDownload } from '$lib/queries/downloads/DownloadMutations.svelte';
	import { createDownloadStream } from '$lib/queries/downloads/DownloadSSE.svelte';
	import { canCancel, derivedDownloadStatus } from '$lib/queries/downloads/downloadStatus';
	import type { DownloadTask } from '$lib/types';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';

	import DownloadProgressBar from './DownloadProgressBar.svelte';
	import DownloadSourceStatus from './DownloadSourceStatus.svelte';
	import DownloadStatusBadge from './DownloadStatusBadge.svelte';
	import VinylProgress from './VinylProgress.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	let { task, showEyebrow = true }: { task: DownloadTask; showEyebrow?: boolean } = $props();

	const stream = createDownloadStream();
	const cancel = cancelDownload();
	$effect(() => {
		if (task.status === 'downloading' || task.status === 'processing') {
			stream.start(task.id);
			return () => stream.stop();
		}
		stream.stop();
	});

	const derivedStatus = $derived(derivedDownloadStatus(task));
	const isSearchingState = $derived(derivedStatus === 'searching');
	const progress = $derived(stream.state.progress);
	const livePct = $derived(progress?.progress_percent ?? task.progress_percent);
	const showBar = $derived(task.status === 'downloading' || task.status === 'processing');
</script>

<Card
	as="section"
	variant="bordered"
	class="now-pressing overflow-hidden"
	aria-label="Now pressing"
>
	<div class="now-pressing-glow" aria-hidden="true"></div>
	{#if showEyebrow}
		<p class="relative mb-4 text-[11px] font-bold uppercase tracking-[0.18em] text-base-content/45">
			Now pressing
		</p>
	{/if}
	<div class="relative flex items-center gap-5">
		<VinylProgress percent={livePct} spinning indeterminate={isSearchingState} size={120} />
		<div class="min-w-0 flex-1">
			<h2 class="truncate text-xl font-black tracking-tight sm:text-2xl">
				<a href={albumHref(task.release_group_mbid)} class="hover:text-primary transition-colors">
					{task.album_title}
				</a>
			</h2>
			<p class="truncate text-sm text-base-content/60">
				{#if task.artist_mbid}
					<a href={artistHref(task.artist_mbid)} class="hover:text-primary transition-colors">
						{task.artist_name}
					</a>
				{:else}
					{task.artist_name}
				{/if}
				{#if task.year}<span class="text-base-content/30"> · </span>{task.year}{/if}
			</p>
			<div class="mt-2"><DownloadStatusBadge {task} /></div>
			{#if showBar}
				<div class="mt-3 max-w-md">
					<DownloadProgressBar
						percent={livePct}
						bytesDownloaded={progress?.bytes_downloaded ?? task.downloaded_bytes}
						bytesTotal={progress?.bytes_total ?? task.total_size_bytes ?? 0}
						filesCompleted={progress?.files_completed ?? task.files_completed}
						filesTotal={progress?.files_total ?? task.files_total}
					/>
				</div>
			{/if}
			<DownloadSourceStatus
				{task}
				live={stream.state.source}
				bytesDownloaded={progress?.bytes_downloaded ?? task.downloaded_bytes}
			/>
			{#if canCancel(task)}
				<Button
					variant="danger-ghost"
					size="xs"
					icon={X}
					class="mt-2"
					onclick={() => cancel.mutate(task.id)}
					disabled={cancel.isPending}
					aria-label="Cancel download"
				>
					Cancel
				</Button>
			{/if}
		</div>
	</div>
</Card>

<style>
	/* the glow pulse rides on the kit Card, outside this component's scope */
	:global(.now-pressing) {
		--hero-glow-color: var(--color-primary-rgb);
		animation: hero-glow 4s ease-in-out infinite;
	}
	.now-pressing-glow {
		position: absolute;
		top: -4rem;
		right: -3rem;
		height: 16rem;
		width: 16rem;
		border-radius: 9999px;
		background: radial-gradient(
			circle,
			oklch(from var(--color-primary) l c h / 0.18),
			transparent 70%
		);
		filter: blur(40px);
		pointer-events: none;
	}
	@media (prefers-reduced-motion: reduce) {
		:global(.now-pressing) {
			animation: none;
		}
	}
</style>
