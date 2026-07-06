<script lang="ts">
	import { RotateCcw, TimerOff, X } from 'lucide-svelte';

	import {
		cancelDownload,
		retryDownload,
		stopAutoRetry
	} from '$lib/queries/downloads/DownloadMutations.svelte';
	import { createDownloadStream } from '$lib/queries/downloads/DownloadSSE.svelte';
	import {
		canCancel,
		canRetry,
		derivedDownloadStatus,
		retryDisplay
	} from '$lib/queries/downloads/downloadStatus';
	import { downloadStatusConfig } from '$lib/queries/downloads/downloadStatusConfig';
	import type { DownloadTask } from '$lib/types';

	import DownloadProgressBar from './DownloadProgressBar.svelte';
	import DownloadStatusBadge from './DownloadStatusBadge.svelte';
	import VinylProgress from './VinylProgress.svelte';

	// the album's in-flight (or actionable failed/partial) download. The Pressing strip - same vinyl
	// language as the Downloads page, so a download reads the same on both screens.
	let { task }: { task: DownloadTask } = $props();

	const cancel = cancelDownload();
	const retry = retryDownload();
	const stopRetry = stopAutoRetry();
	const stream = createDownloadStream();

	// failed/partial waiting on its next auto-retry - the album page's off-switch for the
	// "retry N/M in ~Xm" loop, so it can be stopped without removing the album.
	const isScheduledRetry = $derived(retryDisplay(task)?.kind === 'scheduled');

	const derivedStatus = $derived(derivedDownloadStatus(task));
	const cfg = $derived(downloadStatusConfig[derivedStatus]);
	const isSearching = $derived(derivedStatus === 'searching');
	const isReview = $derived(derivedStatus === 'awaiting_review');
	const isQueued = $derived(derivedStatus === 'queued');
	const isDownloading = $derived(task.status === 'downloading');
	const isProcessing = $derived(task.status === 'processing');
	// the vinyl spins while a task is in flight; it goes indeterminate until there's real progress
	const showVinyl = $derived(isSearching || isQueued || isDownloading || isProcessing);
	const indeterminate = $derived(isSearching || isQueued);
	const showBar = $derived(isDownloading || isProcessing);

	// stream live byte/track progress only while the transfer is moving
	$effect(() => {
		if (task.status === 'downloading' || task.status === 'processing') {
			stream.start(task.id);
			return () => stream.stop();
		}
		stream.stop();
	});

	const progress = $derived(stream.state.progress);
	const livePct = $derived(progress?.progress_percent ?? task.progress_percent);
	// the orchestrator emits a 'retrying' status while it fails over to another peer
	const isRetrying = $derived(stream.state.status === 'retrying');
</script>

<div
	class="press-strip flex items-center gap-3 rounded-2xl border border-base-content/5 bg-base-200/60 p-2.5 pr-3 backdrop-blur-sm"
	role="status"
	aria-live="polite"
>
	<div class="shrink-0">
		{#if showVinyl}
			<VinylProgress percent={livePct} {indeterminate} spinning size={44} />
		{:else}
			<div class="grid size-11 place-items-center rounded-full bg-base-300/70 text-base-content/70">
				<cfg.icon class="h-5 w-5" aria-hidden="true" />
			</div>
		{/if}
	</div>

	<div class="min-w-0 flex-1">
		<DownloadStatusBadge {task} />
		{#if showBar}
			<div class="mt-1.5 max-w-sm">
				<DownloadProgressBar
					percent={livePct}
					bytesDownloaded={progress?.bytes_downloaded ?? task.downloaded_bytes}
					bytesTotal={progress?.bytes_total ?? task.total_size_bytes ?? 0}
					filesCompleted={progress?.files_completed ?? task.files_completed}
					filesTotal={progress?.files_total ?? task.files_total}
				/>
			</div>
		{/if}
		{#if isRetrying}
			<p class="mt-1 text-xs text-base-content/60">Trying another source…</p>
		{/if}
		{#if task.error_message}
			<p class="mt-1 line-clamp-2 text-xs text-error/80">{task.error_message}</p>
		{/if}
	</div>

	<div class="flex shrink-0 items-center gap-1.5">
		{#if isReview}
			<a href="/downloads" class="btn btn-primary btn-xs">Review</a>
		{/if}
		{#if canCancel(task)}
			<button
				class="btn btn-ghost btn-xs text-error/70 hover:text-error"
				onclick={() => cancel.mutate(task.id)}
				disabled={cancel.isPending}
				aria-label="Cancel download"
			>
				<X class="h-3.5 w-3.5" /> Cancel
			</button>
		{/if}
		{#if canRetry(task)}
			<button
				class="btn btn-ghost btn-primary btn-xs"
				onclick={() => retry.mutate(task.id)}
				disabled={retry.isPending}
				aria-label="Retry download"
			>
				<RotateCcw class="h-3.5 w-3.5" /> Retry
			</button>
		{/if}
		{#if isScheduledRetry}
			<button
				class="btn btn-ghost btn-xs text-base-content/60 hover:text-error"
				onclick={() => stopRetry.mutate(task.id)}
				disabled={stopRetry.isPending}
				title="Stop auto-retrying - it won't be watched for later either"
				aria-label="Stop auto-retrying this download"
			>
				<TimerOff class="h-3.5 w-3.5" /> Stop retrying
			</button>
		{/if}
	</div>
</div>
