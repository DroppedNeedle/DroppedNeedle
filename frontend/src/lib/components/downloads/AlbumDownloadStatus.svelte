<script lang="ts">
	import { FolderLock, RotateCcw, TimerOff, X } from 'lucide-svelte';

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
	import type { DownloadTask, HeldFile } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	import DownloadProgressBar from './DownloadProgressBar.svelte';
	import DownloadReasonNote from './DownloadReasonNote.svelte';
	import DownloadSourceStatus from './DownloadSourceStatus.svelte';
	import DownloadStatusBadge from './DownloadStatusBadge.svelte';
	import VinylProgress from './VinylProgress.svelte';

	// the album's in-flight (or actionable failed/partial) download. The Pressing strip - same vinyl
	// language as the Downloads page, so a download reads the same on both screens.
	interface Props {
		task: DownloadTask;
		/** Files of this download held for a decision. */
		heldFiles?: HeldFile[];
	}

	let { task, heldFiles = [] }: Props = $props();

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
	const isHeld = $derived(heldFiles.length > 0);
	const heldReason = $derived(heldFiles[0]?.reason_text ?? null);
	const keptFileLabel = $derived(
		`${heldFiles.length} ${heldFiles.length === 1 ? 'file' : 'files'} held`
	);
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

<Card variant="bordered" padding="sm" role="status" aria-live="polite">
	<div class="flex items-center gap-3">
		<div class="shrink-0">
			{#if showVinyl}
				<VinylProgress percent={livePct} {indeterminate} spinning size={44} />
			{:else}
				<div
					class="grid size-11 place-items-center rounded-full bg-base-300/70 text-base-content/70"
				>
					{#if isHeld}
						<FolderLock class="h-5 w-5 text-warning" aria-hidden="true" />
					{:else}
						<cfg.icon class="h-5 w-5" aria-hidden="true" />
					{/if}
				</div>
			{/if}
		</div>

		<div class="min-w-0 flex-1">
			{#if isHeld}
				<p class="text-xs font-bold text-warning">{keptFileLabel}</p>
				{#if heldReason}
					<p class="mt-1 line-clamp-2 text-xs leading-relaxed text-base-content/65">
						{heldReason}
					</p>
				{/if}
			{:else}
				<DownloadStatusBadge {task} />
			{/if}
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
			{#if !isReview}
				<DownloadSourceStatus
					{task}
					live={stream.state.source}
					bytesDownloaded={progress?.bytes_downloaded ?? task.downloaded_bytes}
					compact
				/>
				<DownloadReasonNote {task} />
			{/if}
		</div>

		<div class="flex shrink-0 items-center gap-1.5">
			{#if isReview}
				<Button variant="primary" size="xs" href={withBasePath('/downloads')}>Review</Button>
			{/if}
			{#if canCancel(task)}
				<Button
					variant="danger-ghost"
					size="xs"
					icon={X}
					onclick={() => cancel.mutate(task.id)}
					disabled={cancel.isPending}
					aria-label="Cancel download"
				>
					Cancel
				</Button>
			{/if}
			{#if canRetry(task)}
				<Button
					variant="ghost"
					size="xs"
					icon={RotateCcw}
					onclick={() => retry.mutate(task.id)}
					disabled={retry.isPending}
					aria-label="Retry download"
				>
					Retry
				</Button>
			{/if}
			{#if isScheduledRetry}
				<Button
					variant="ghost"
					size="xs"
					icon={TimerOff}
					onclick={() => stopRetry.mutate(task.id)}
					disabled={stopRetry.isPending}
					title="Stop auto-retrying - it won't be watched for later either"
					aria-label="Stop auto-retrying this download"
				>
					Stop retrying
				</Button>
			{/if}
		</div>
	</div>
</Card>
