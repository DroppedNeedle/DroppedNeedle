<script lang="ts">
	import { Clock3, ListOrdered, SkipForward } from 'lucide-svelte';

	import {
		NO_FALLBACK_COPY,
		fallbackCopy,
		labelForQualityStep,
		nextPreferenceLabel
	} from '$lib/utils/acquisitionLabels';
	import { withBasePath } from '$lib/utils/basePath';

	import { tryNextSource } from '$lib/queries/downloads/DownloadMutations.svelte';
	import type { DownloadSourceUpdate } from '$lib/queries/downloads/DownloadSSE.svelte';
	import type { DownloadTask } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		task: DownloadTask;
		live?: DownloadSourceUpdate | null;
		bytesDownloaded?: number;
		compact?: boolean;
	}

	let {
		task,
		live = null,
		bytesDownloaded = task.downloaded_bytes,
		compact = false
	}: Props = $props();

	const nextSource = tryNextSource();
	let now = $state(Date.now());
	let actionMessage = $state<string | null>(null);
	let actionCandidate = $state<string | null>(null);

	const candidateIndex = $derived(live ? live.candidate_index : task.candidate_index);
	const source = $derived(live?.source ?? task.source);
	const qualityFormat = $derived(
		live
			? ((live as unknown as { quality_format?: string | null }).quality_format ??
					task.quality_format)
			: task.quality_format
	);
	const qualityBitDepth = $derived(live ? live.quality_bit_depth : task.quality_bit_depth);
	const qualitySampleRate = $derived(live ? live.quality_sample_rate : task.quality_sample_rate);
	const advertisedQueueDepth = $derived(
		live ? live.advertised_queue_depth : task.advertised_queue_depth
	);
	const queueStart = $derived(live ? live.queue_position_start : task.queue_position_start);
	const queueEnd = $derived(live ? live.queue_position_end : task.queue_position_end);
	const remoteQueued = $derived(live ? live.remote_queued : task.remote_queued);
	const fallbackAt = $derived(
		live ? live.preferred_quality_fallback_at : task.preferred_quality_fallback_at
	);
	const attemptNumber = $derived(live?.attempt_number || task.attempt_number);
	const attemptTotal = $derived(live?.attempt_total || task.attempt_total);
	const hasNext = $derived(live?.has_next_source ?? task.has_next_source);
	const isQueued = $derived(
		source === 'soulseek' && remoteQueued && task.status === 'downloading' && bytesDownloaded === 0
	);
	const canTryNext = $derived(isQueued && hasNext && candidateIndex !== null);

	const quality = $derived.by(() => {
		const parts: string[] = [];
		if (qualityBitDepth) parts.push(`${qualityBitDepth}-bit`);
		if (qualitySampleRate) {
			const khz = qualitySampleRate / 1000;
			parts.push(`${Number.isInteger(khz) ? khz.toFixed(0) : khz.toFixed(1)} kHz`);
		}
		const resolution = parts.join(' / ');
		const format = qualityFormat?.toUpperCase() ?? '';
		return [resolution, format].filter(Boolean).join(' ');
	});

	const queueLabel = $derived.by(() => {
		if (advertisedQueueDepth != null) {
			return `queue ${advertisedQueueDepth.toLocaleString()}`;
		}
		if (queueStart != null) {
			return `position ${queueStart.toLocaleString()}`;
		}
		return 'remote queue';
	});

	const livePosition = $derived.by(() => {
		if (queueStart == null) return null;
		if (queueEnd == null || queueEnd === queueStart) return queueStart.toLocaleString();
		return `${queueStart.toLocaleString()}–${queueEnd.toLocaleString()}`;
	});

	const fallbackMinutes = $derived(
		fallbackAt == null ? null : Math.max(0, Math.ceil((fallbackAt * 1000 - now) / 60_000))
	);

	const nextPreferenceName = $derived(
		nextPreferenceLabel({
			format: qualityFormat,
			bitrate: task.quality_bitrate,
			bit_depth: qualityBitDepth,
			sample_rate: qualitySampleRate
		})
	);
	// Replacement for the old hard-coded "Lower-quality fallback in Nm" line; when a
	// deadline never starts and no further source exists we explicitly say so.
	const fallbackLine = $derived.by(() => {
		if (!isQueued) return null;
		if (fallbackMinutes !== null) return fallbackCopy(nextPreferenceName, fallbackMinutes);
		return hasNext ? null : NO_FALLBACK_COPY;
	});
	const awaitingReview = $derived(
		task.status === 'queued' &&
			Boolean(task.search_job_id) &&
			task.candidate_index === null &&
			!task.held_for_review
	);
	const evidenceChips = $derived.by(() => {
		const chips: string[] = [];
		if (task.quality_bitrate) chips.push(`${task.quality_bitrate} kbps`);
		if (typeof task.quality_preference_step === 'number') {
			chips.push(labelForQualityStep(task.quality_preference_step, null));
		}
		if (task.quality_certainty) {
			chips.push(
				`Certainty: ${task.quality_certainty[0].toUpperCase()}${task.quality_certainty.slice(1)}`
			);
		}
		if (task.quality_provenance) {
			chips.push(`Provenance: ${task.quality_provenance.replaceAll('_', ' ')}`);
		}
		if (task.manual_quality_override) chips.push('Manual pick');
		return chips;
	});

	$effect(() => {
		if (fallbackAt == null) return;
		const timer = window.setInterval(() => (now = Date.now()), 30_000);
		return () => window.clearInterval(timer);
	});

	function onTryNext() {
		if (candidateIndex == null) return;
		actionMessage = null;
		actionCandidate = `${source ?? ''}:${candidateIndex}`;
		nextSource.mutate(
			{ id: task.id, candidateIndex },
			{
				onError: (error: unknown) => {
					actionMessage =
						error instanceof Error && error.message
							? error.message
							: 'Could not switch to the next source.';
				}
			}
		);
	}

	$effect(() => {
		const current = `${source ?? ''}:${candidateIndex ?? ''}`;
		if (actionCandidate && actionCandidate !== current) {
			actionCandidate = null;
			actionMessage = null;
			nextSource.reset();
		}
	});
</script>

<!-- Awaiting review is a display-only derivation: queued + search job + no picked
		candidate means only unknown-resolution copies were found (Acquisition plan). -->
{#if awaitingReview}
	<div
		class="mt-2 rounded-box border border-base-content/15 bg-base-200 px-3 py-2 text-xs"
		role="status"
	>
		<p class="font-semibold text-warning">
			Waiting: only unknown-resolution copies were found - review required
		</p>
		<a href={withBasePath('/downloads')} class="link link-hover font-semibold text-primary">
			Review candidates
		</a>
	</div>
{/if}
<!-- Plugin sources (source "plugin:<name>") intentionally take the generic path:
	no remote-queue hints below - only soulseek peers report queue telemetry. -->
{#if source === 'soulseek' && task.status === 'downloading' && !task.held_for_review}
	<div
		class:source-telemetry-compact={compact}
		class="source-telemetry mt-2"
		role="status"
		aria-live="polite"
		aria-atomic="true"
	>
		<div class="min-w-0">
			{#if quality}
				<p class="truncate text-xs font-bold tracking-wide text-base-content/85">{quality}</p>
			{/if}
			<p class="mt-0.5 text-xs text-base-content/65">
				{isQueued ? `Waiting for Soulseek · ${queueLabel}` : 'Downloading from Soulseek'}
			</p>
		</div>

		<div class="source-facts">
			{#if attemptNumber > 0 && attemptTotal > 0}
				<span
					><ListOrdered class="size-3" aria-hidden="true" /> Trying source {attemptNumber} of {attemptTotal}</span
				>
			{/if}
			{#if isQueued && livePosition}
				<span>Live position {livePosition}</span>
			{/if}
			{#if isQueued && fallbackLine}
				<span class="text-warning">
					<Clock3 class="size-3" aria-hidden="true" />
					{fallbackLine}
				</span>
			{/if}
			{#each evidenceChips as chip (chip)}
				<span class="badge badge-ghost badge-sm">{chip}</span>
			{/each}
			{#if task.quality_snapshot_summary}
				<span class="max-w-full truncate" title={task.quality_snapshot_summary}>
					{task.quality_snapshot_summary}
				</span>
			{/if}
		</div>

		{#if canTryNext}
			<div class="source-next">
				<Button
					variant="outline"
					size="xs"
					icon={SkipForward}
					onclick={onTryNext}
					disabled={nextSource.isPending}
					aria-label="Try the next ranked download source"
				>
					{nextSource.isPending ? 'Switching…' : 'Try next source'}
				</Button>
			</div>
		{/if}
		{#if actionMessage}
			<p class="source-action-error" role="alert">{actionMessage}</p>
		{/if}
	</div>
{/if}

<style>
	.source-telemetry {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.5rem 1rem;
		max-width: 42rem;
		padding: 0.65rem 0.75rem;
		border-left: 2px solid var(--color-primary);
		border-radius: 0.25rem 0.75rem 0.75rem 0.25rem;
		background: oklch(from var(--color-base-300) l c h / 0.42);
	}
	.source-facts {
		display: flex;
		grid-column: 1 / -1;
		flex-wrap: wrap;
		gap: 0.35rem 0.85rem;
		font-size: 0.6875rem;
		color: oklch(from var(--color-base-content) l c h / 0.58);
	}
	.source-facts span {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
	}
	.source-next {
		grid-column: 2;
		grid-row: 1;
	}
	.source-action-error {
		grid-column: 1 / -1;
		margin: 0;
		font-size: 0.75rem;
		font-weight: 600;
		color: var(--color-error);
	}
	.source-telemetry-compact {
		padding: 0.5rem 0.625rem;
	}
	@media (max-width: 40rem) {
		.source-telemetry {
			grid-template-columns: minmax(0, 1fr);
		}
		.source-next {
			grid-column: 1;
			grid-row: auto;
			justify-self: start;
		}
	}
</style>
