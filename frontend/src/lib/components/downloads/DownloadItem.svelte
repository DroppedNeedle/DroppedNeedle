<script lang="ts">
	import { ExternalLink, RotateCcw, X } from 'lucide-svelte';

	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { cancelDownload, retryDownload } from '$lib/queries/downloads/DownloadMutations.svelte';
	import { createDownloadStream } from '$lib/queries/downloads/DownloadSSE.svelte';
	import {
		canCancel,
		canRetry,
		derivedDownloadStatus
	} from '$lib/queries/downloads/downloadStatus';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { DownloadTask } from '$lib/types';
	import { albumHref } from '$lib/utils/entityRoutes';

	import DownloadProgressBar from './DownloadProgressBar.svelte';
	import DownloadStatusBadge from './DownloadStatusBadge.svelte';
	import ReviewCandidates from './ReviewCandidates.svelte';
	import VinylProgress from './VinylProgress.svelte';

	let { task }: { task: DownloadTask } = $props();

	const cancel = cancelDownload();
	const retry = retryDownload();
	const stream = createDownloadStream();

	const derivedStatus = $derived(derivedDownloadStatus(task));
	const isSearchingState = $derived(derivedStatus === 'searching');
	const isReview = $derived(derivedStatus === 'awaiting_review');
	const isProcessing = $derived(task.status === 'processing');
	const isDownloading = $derived(task.status === 'downloading');
	const isLive = $derived(isSearchingState || isDownloading || isProcessing);
	const showBar = $derived(isDownloading || isProcessing);
	const isCompleted = $derived(task.status === 'completed' || task.status === 'partial');

	// only stream live progress while the transfer is moving
	$effect(() => {
		if (task.status === 'downloading' || task.status === 'processing') {
			stream.start(task.id);
			return () => stream.stop();
		}
		stream.stop();
	});

	const progress = $derived(stream.state.progress);
	const livePct = $derived(progress?.progress_percent ?? task.progress_percent);
	const isOwnedByOther = $derived(authStore.isAdmin && task.user_id !== authStore.user?.id);

	let reviewOpen = $state(false);
	// Once retry is clicked the row is about to move to the active queue (the failed task
	// is superseded by a new attempt); keep the button disabled until then so a second
	// click can't spawn a duplicate. Reset only if the retry call itself fails.
	let retried = $state(false);
	function onRetry() {
		retried = true;
		retry.mutate(task.id, { onError: () => (retried = false) });
	}
</script>

<article
	class="dl-item group rounded-2xl border border-base-content/5 bg-base-200/50 p-3 backdrop-blur-sm"
>
	<div class="flex items-center gap-3 sm:gap-4">
		<div class="shrink-0">
			{#if isSearchingState}
				<VinylProgress indeterminate spinning size={64} />
			{:else if isLive}
				<VinylProgress percent={livePct} spinning size={64} />
			{:else}
				<div class="size-16 overflow-hidden rounded-lg ring-1 ring-base-content/10">
					<AlbumImage
						mbid={task.release_group_mbid}
						alt={task.album_title}
						size="sm"
						rounded="lg"
						className="h-full w-full"
					/>
				</div>
			{/if}
		</div>

		<div class="min-w-0 flex-1">
			<h3 class="truncate text-sm font-semibold sm:text-base">{task.album_title}</h3>
			<p class="truncate text-xs text-base-content/60">
				{task.artist_name}
				{#if task.year}<span class="text-base-content/30"> · </span>{task.year}{/if}
				{#if task.download_type === 'track' && task.track_title}
					<span class="text-base-content/30"> · </span>{task.track_title}
				{/if}
			</p>
			<div class="mt-1 flex flex-wrap items-center gap-1.5">
				<DownloadStatusBadge {task} />
				{#if isOwnedByOther}
					<span class="text-[11px] text-base-content/50">(another user's download)</span>
				{/if}
			</div>
			{#if showBar}
				<div class="mt-2 max-w-md">
					<DownloadProgressBar
						percent={livePct}
						bytesDownloaded={progress?.bytes_downloaded ?? task.downloaded_bytes}
						bytesTotal={progress?.bytes_total ?? task.total_size_bytes ?? 0}
						filesCompleted={progress?.files_completed ?? task.files_completed}
						filesTotal={progress?.files_total ?? task.files_total}
					/>
				</div>
			{/if}
			{#if task.error_message}
				<p class="mt-1 line-clamp-2 text-xs text-error/80">{task.error_message}</p>
			{/if}
		</div>

		<div class="flex shrink-0 flex-col items-end gap-1.5">
			{#if isReview}
				<button
					class="btn btn-primary btn-xs"
					onclick={() => (reviewOpen = !reviewOpen)}
					aria-expanded={reviewOpen}
				>
					{reviewOpen ? 'Hide' : 'Review'}
				</button>
			{/if}
			{#if canCancel(task)}
				<button
					class="btn btn-ghost btn-xs text-error/70 hover:text-error"
					onclick={() => cancel.mutate(task.id)}
					disabled={cancel.isPending}
					title="Cancel download"
					aria-label="Cancel download"
				>
					<X class="h-3.5 w-3.5" /> Cancel
				</button>
			{:else if isProcessing}
				<button
					class="btn btn-ghost btn-xs"
					disabled
					title="Can't cancel while processing files."
					aria-label="Cancel unavailable while processing files"
				>
					<X class="h-3.5 w-3.5" /> Cancel
				</button>
			{/if}
			{#if canRetry(task)}
				<button
					class="btn btn-ghost btn-primary btn-xs"
					onclick={onRetry}
					disabled={retry.isPending || retried}
					title="Retry"
					aria-label="Retry download"
				>
					<RotateCcw class="h-3.5 w-3.5" /> {retried ? 'Retrying…' : 'Retry'}
				</button>
			{/if}
			{#if isCompleted}
				<a
					href={albumHref(task.release_group_mbid)}
					class="btn btn-ghost btn-xs"
					aria-label="View in library"
				>
					<ExternalLink class="h-3.5 w-3.5" /> View in Library
				</a>
			{/if}
		</div>
	</div>

	{#if isReview && reviewOpen}
		<ReviewCandidates {task} />
	{/if}
</article>

<style>
	.dl-item {
		transition:
			transform 0.2s var(--ease-spring, ease-out),
			box-shadow 0.2s var(--ease-spring, ease-out),
			border-color 0.2s ease;
	}
	.dl-item:hover {
		transform: translateY(-2px);
		box-shadow: 0 12px 24px -14px oklch(from var(--color-base-100) l c h / 0.9);
		border-color: oklch(from var(--color-primary) l c h / 0.2);
	}
	@media (prefers-reduced-motion: reduce) {
		.dl-item {
			transition: none;
		}
		.dl-item:hover {
			transform: none;
		}
	}
</style>
