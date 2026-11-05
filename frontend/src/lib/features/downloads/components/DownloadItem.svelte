<script lang="ts">
	import {
		Archive,
		ArrowUp,
		Clock3,
		ExternalLink,
		FileDown,
		RotateCcw,
		Search,
		TimerOff,
		TriangleAlert,
		X
	} from 'lucide-svelte';

	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import {
		createCancelDownloadMutation,
		createReimportDownloadMutation,
		createRetryDownloadMutation,
		createStopAutoRetryMutation,
		retryJoinedRunning
	} from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import { createDownloadStream } from '$lib/features/downloads/queries/DownloadSSE.svelte';
	import { createStartAlbumSearchMutation } from '$lib/features/downloads/queries/SearchQueries.svelte';
	import {
		canCancel,
		canReimport,
		canRetry,
		derivedDownloadStatus,
		retryDisplay
	} from '$lib/features/downloads/queries/downloadStatus';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { DownloadTask } from '$lib/features/downloads/queries/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import {
		EMPTY_STATE_COPY,
		classifyEmptyState
	} from '$lib/features/downloads/components/acquisitionLabels';

	import DownloadProgressBar from './DownloadProgressBar.svelte';
	import DownloadReasonNote from './DownloadReasonNote.svelte';
	import DownloadSourceStatus from './DownloadSourceStatus.svelte';
	import DownloadStatusBadge from './DownloadStatusBadge.svelte';
	import ManualSearch from './ManualSearch.svelte';
	import VinylProgress from './VinylProgress.svelte';

	interface Props {
		task: DownloadTask;
		/** The in-flight download this one gave way to, when it is on the page. */
		liveTaskId?: string | null;
	}

	let { task, liveTaskId = null }: Props = $props();

	const cancel = createCancelDownloadMutation();
	const retry = createRetryDownloadMutation();
	const stopRetry = createStopAutoRetryMutation();
	const reimport = createReimportDownloadMutation();
	const search = createStartAlbumSearchMutation();
	const stream = createDownloadStream();

	// A failed/partial task waiting on its next auto-retry: offer an off-switch so the
	// "retry N/M in ~Xm" loop can be stopped without removing the album.
	const isScheduledRetry = $derived(retryDisplay(task)?.kind === 'scheduled');

	const derivedStatus = $derived(derivedDownloadStatus(task));
	const isSearchingState = $derived(derivedStatus === 'searching');
	const isReview = $derived(derivedStatus === 'awaiting_review');
	const isProcessing = $derived(task.status === 'processing');
	const isDownloading = $derived(task.status === 'downloading');
	const isLive = $derived(isSearchingState || isDownloading || isProcessing);
	const showBar = $derived(isDownloading || isProcessing);
	const isCompleted = $derived(task.status === 'completed' || task.status === 'partial');
	const cleanupState = $derived(task.acquisition_cleanup_state);

	const emptyState = $derived(task.held_for_review ? null : classifyEmptyState(task));
	const emptyCopy = $derived(emptyState ? EMPTY_STATE_COPY[emptyState] : null);

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

	// A task parked on a search job waits for a pick from that job.
	const parkedJobId = $derived(
		isReview && !task.held_for_review && task.search_job_id ? task.search_job_id : null
	);
	let reviewOpen = $state(false);
	// A manual search started from this row (failed, cancelled or partial
	// album downloads): every source's candidates for the album, to pick from.
	let manualJobId = $state<string | null>(null);
	const canSearchManually = $derived(
		canRetry(task) && task.download_type !== 'track' && !!task.release_group_mbid
	);
	function onSearchManually() {
		search.mutate(
			{
				artist_name: task.artist_name,
				album_title: task.album_title,
				year: task.year,
				release_group_mbid: task.release_group_mbid,
				release_mbid: task.release_mbid
			},
			{
				onSuccess: (data) => {
					if (data.job_id) manualJobId = data.job_id;
				}
			}
		);
	}
	// Once retry is clicked the row is about to move to the active queue (the failed task
	// is superseded by a new attempt); keep the button disabled until then so a second
	// click can't spawn a duplicate. Reset if the retry call fails, or if it joined a
	// download already running: then this row stays and nothing new is coming.
	let retried = $state(false);
	function onRetry() {
		retried = true;
		retry.mutate(task.id, {
			onSuccess: (data) => {
				if (retryJoinedRunning(task.id, data.task_id)) retried = false;
			},
			onError: () => (retried = false)
		});
	}
</script>

<Card as="article" variant="bordered" padding="none" id="download-{task.id}" class="scroll-mt-24">
	<div class="row">
		<div class="row__art">
			{#if isSearchingState}
				<VinylProgress indeterminate spinning size={88} />
			{:else if isLive}
				<VinylProgress percent={livePct} spinning={isDownloading} size={88} />
			{:else}
				<div class="row__cover">
					<AlbumImage
						mbid={task.release_group_mbid}
						alt={task.album_title}
						size="full"
						requestSize={250}
						rounded="none"
						className="h-full w-full"
					/>
				</div>
			{/if}
		</div>

		<div class="row__main">
			<h3 class="row__title">
				<a href={albumHref(task.release_group_mbid)} class="row__link">{task.album_title}</a>
			</h3>
			<p class="row__by">
				{#if task.artist_mbid}
					<a href={artistHref(task.artist_mbid)} class="row__artist">{task.artist_name}</a>
				{:else}
					{task.artist_name}
				{/if}
				{#if task.year}<span class="text-muted mr-1.5 ml-0.5">·</span>{task.year}{/if}
				{#if task.download_type === 'track' && task.track_title}
					<span class="text-muted mr-1.5 ml-0.5">·</span>{task.track_title}
				{/if}
			</p>
			<div class="row__chips">
				<DownloadStatusBadge {task} />
				{#if task.source === 'usenet'}
					<StatusChip size="xs">Usenet</StatusChip>
				{/if}
				{#if task.source === 'usenet' && task.download_type === 'track'}
					<span title="A whole album NZB was fetched to extract this one track">
						<StatusChip size="xs">via album NZB</StatusChip>
					</span>
				{/if}
				<!-- Plugin sources take the generic path: no client-specific badges or
					remote-queue hints, just the source key. -->
				{#if task.source && task.source !== 'soulseek' && task.source !== 'usenet'}
					<StatusChip size="xs">{task.source}</StatusChip>
				{/if}
				{#if isOwnedByOther}
					<span class="row__note">(another user's download)</span>
				{/if}
				{#if cleanupState === 'pending'}
					<span class="row__note inline-flex items-center gap-1">
						<Clock3 class="size-3" aria-hidden="true" /> Removing temporary files
					</span>
				{:else if cleanupState === 'preserved'}
					<span
						class="row__note inline-flex items-center gap-1 text-warning"
						title="Kept on purpose by your library settings. Your music is imported."
					>
						<Archive class="size-3" aria-hidden="true" /> Temporary files kept
					</span>
				{:else if cleanupState === 'needs_attention'}
					<span
						class="row__note inline-flex items-center gap-1 text-error"
						title="Your library is safe. The temporary copy was kept because it could not be verified safe to remove. DroppedNeedle retries automatically."
					>
						<TriangleAlert class="size-3" aria-hidden="true" /> Couldn't remove temporary files
					</span>
				{/if}
			</div>
			{#if showBar}
				<div class="mt-3 max-w-xl">
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
				compact
			/>
			<DownloadReasonNote {task} />
			{#if emptyCopy}
				<div class="row__empty">
					<p class="font-semibold">{emptyCopy.title}</p>
					<p class="text-muted">{emptyCopy.detail}</p>
				</div>
			{/if}
		</div>

		<div class="row__actions">
			{#if parkedJobId}
				<Button
					variant="primary"
					size="display-sm"
					onclick={() => (reviewOpen = !reviewOpen)}
					aria-expanded={reviewOpen}
				>
					{reviewOpen ? 'Hide' : 'Review'}
				</Button>
			{/if}
			{#if canCancel(task)}
				<Button
					variant="outline"
					size="sm"
					icon={X}
					onclick={() => cancel.mutate(task.id)}
					disabled={cancel.isPending}
					title="Cancel download"
					aria-label="Cancel download"
				>
					Cancel
				</Button>
			{:else if isProcessing}
				<Button
					variant="outline"
					size="sm"
					icon={X}
					disabled
					title="Can't cancel while processing files."
					aria-label="Cancel unavailable while processing files"
				>
					Cancel
				</Button>
			{/if}
			{#if authStore.isAdmin && canReimport(task)}
				<Button
					variant="ghost"
					size="sm"
					icon={FileDown}
					onclick={() =>
						reimport.mutate({ id: task.id, release_group_mbid: task.release_group_mbid })}
					disabled={reimport.isPending}
					title="Already fixed this in the download client? Check the downloads mount again without re-searching."
					aria-label="Retry import"
				>
					{reimport.isPending ? 'Checking...' : 'Retry import'}
				</Button>
			{/if}
			{#if canSearchManually}
				<Button
					variant="ghost"
					size="sm"
					icon={Search}
					onclick={onSearchManually}
					disabled={search.isPending || manualJobId !== null}
					title="Search every download source for this album and choose the copy yourself"
					aria-label="Search sources and pick a copy"
				>
					{search.isPending ? 'Starting…' : 'Pick a source'}
				</Button>
			{/if}
			{#if liveTaskId}
				<Button variant="ghost" size="sm" icon={ArrowUp} href="#download-{liveTaskId}">
					See the download
				</Button>
			{:else if canRetry(task)}
				<Button
					variant="outline"
					size="sm"
					icon={RotateCcw}
					onclick={onRetry}
					disabled={retry.isPending || retried}
					title="Retry"
					aria-label="Retry download"
				>
					{retried ? 'Retrying…' : 'Retry'}
				</Button>
			{/if}
			{#if isScheduledRetry}
				<Button
					variant="ghost"
					size="sm"
					icon={TimerOff}
					onclick={() => stopRetry.mutate(task.id)}
					disabled={stopRetry.isPending}
					title="Stop auto-retrying - it won't be watched for later either"
					aria-label="Stop auto-retrying this download"
				>
					Stop retrying
				</Button>
			{/if}
			{#if isCompleted}
				<Button
					variant="ghost"
					size="sm"
					icon={ExternalLink}
					href={albumHref(task.release_group_mbid)}
					aria-label="View in library"
				>
					View in Library
				</Button>
			{/if}
		</div>
	</div>

	{#if parkedJobId && reviewOpen}
		<div class="row__results">
			<ManualSearch jobId={parkedJobId} onClose={() => (reviewOpen = false)} />
		</div>
	{:else if manualJobId}
		<div class="row__results">
			<ManualSearch jobId={manualJobId} onClose={() => (manualJobId = null)} />
		</div>
	{/if}
</Card>

<style>
	.row {
		display: grid;
		grid-template-columns: 5.5rem minmax(0, 1fr) auto;
		align-items: center;
		gap: 1rem 1.75rem;
		padding: 0.875rem 1.5rem 0.875rem 1.25rem;
	}

	.row__art {
		display: grid;
		place-items: center;
		width: 5.5rem;
		height: 5.5rem;
	}
	.row__cover {
		width: 100%;
		height: 100%;
		overflow: hidden;
		border-radius: 0.25rem;
	}

	.row__main {
		min-width: 0;
	}
	.row__title {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 1.25rem;
		font-weight: 700;
		line-height: 1.25;
	}
	.row__link {
		text-decoration: underline;
		text-decoration-thickness: 2px;
		text-underline-offset: 4px;
		border-radius: 0.25rem;
	}
	.row__link:hover,
	.row__artist:hover {
		color: var(--dn-title);
	}
	.row__by {
		margin-top: 0.125rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 0.9375rem;
	}
	.row__chips {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.375rem 0.625rem;
		margin-top: 0.5rem;
	}
	.row__note {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		color: var(--dn-muted);
	}
	.row__empty {
		margin-top: 0.5rem;
		padding: 0.5rem 0.75rem;
		border: 1px solid var(--dn-line);
		border-radius: var(--radius-field);
		font-size: 0.75rem;
	}

	.row__actions {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.5rem;
	}

	.row__results {
		padding: 0 1.25rem 1.25rem;
	}

	@media (max-width: 639px) {
		.row {
			grid-template-columns: 4.5rem minmax(0, 1fr);
			gap: 0.75rem 1rem;
			padding: 0.875rem 1rem;
		}
		.row__art {
			width: 4.5rem;
			height: 4.5rem;
			align-self: start;
		}
		.row__art :global(.vinyl) {
			transform: scale(0.82);
		}
		.row__title {
			font-size: 1.0625rem;
		}
		.row__by {
			font-size: 0.875rem;
		}
		.row__actions {
			grid-column: 1 / -1;
			flex-direction: row;
			flex-wrap: wrap;
			justify-content: flex-end;
		}
		.row__results {
			padding: 0 0.75rem 0.75rem;
		}
	}
</style>
