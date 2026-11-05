<script lang="ts">
	import { Bird, Clock3, ListOrdered, Newspaper, Puzzle, SkipForward } from 'lucide-svelte';

	import { NO_FALLBACK_COPY } from '$lib/features/downloads/components/acquisitionLabels';

	import { createTryNextSourceMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import type { DownloadSourceUpdate } from '$lib/features/downloads/queries/DownloadSSE.svelte';
	import type { DownloadTask } from '$lib/features/downloads/queries/types';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		task: DownloadTask;
		live?: DownloadSourceUpdate | null;
		bytesDownloaded?: number;
		compact?: boolean;
		/** The hero's side panel: the source's name and its facts as a list. */
		panel?: boolean;
	}

	let {
		task,
		live = null,
		bytesDownloaded = task.downloaded_bytes,
		compact = false,
		panel = false
	}: Props = $props();

	const nextSource = createTryNextSourceMutation();
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

	const fallbackLine = $derived(isQueued && !hasNext ? NO_FALLBACK_COPY : null);
	const evidenceChips = $derived.by(() => {
		const chips: string[] = [];
		if (task.quality_bitrate) chips.push(`${task.quality_bitrate} kbps`);
		return chips;
	});

	// The hero's panel names the source and lists only the facts this download has.
	const sourceName = $derived(
		source === 'soulseek'
			? 'Soulseek'
			: source === 'usenet'
				? 'Usenet'
				: (source?.replace(/^plugin:/, '') ?? null)
	);
	const SourceMark = $derived(
		source === 'soulseek' ? Bird : source === 'usenet' ? Newspaper : Puzzle
	);
	const peer = $derived(source === 'soulseek' ? task.source_username || null : null);
	const panelFacts = $derived(
		[
			peer ? { key: 'User', value: peer } : null,
			quality ? { key: 'Quality', value: quality } : null,
			evidenceChips[0] ? { key: 'Bitrate', value: evidenceChips[0] } : null,
			attemptNumber > 0 && attemptTotal > 0
				? { key: 'Trying', value: `${attemptNumber} of ${attemptTotal}` }
				: null,
			isQueued ? { key: 'Queue', value: livePosition ?? queueLabel } : null
		].filter((fact) => fact !== null)
	);

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

{#snippet tryNext()}
	{#if canTryNext}
		<Button
			variant={panel ? 'paper' : 'outline'}
			size="xs"
			icon={SkipForward}
			onclick={onTryNext}
			disabled={nextSource.isPending}
			aria-label="Try the next ranked download source"
		>
			{nextSource.isPending ? 'Switching…' : 'Try next source'}
		</Button>
	{/if}
	{#if actionMessage}
		<p class="source-action-error" role="alert">{actionMessage}</p>
	{/if}
{/snippet}

{#if panel}
	{#if sourceName && !task.held_for_review}
		<div class="source-panel" role="status" aria-live="polite" aria-atomic="true">
			<p class="source-panel__label">Source</p>
			<p class="source-panel__name">
				<SourceMark class="size-5" aria-hidden="true" />
				{sourceName}
			</p>
			{#if panelFacts.length > 0}
				<dl class="source-panel__facts">
					{#each panelFacts as fact (fact.key)}
						<dt>{fact.key}</dt>
						<dd>{fact.value}</dd>
					{/each}
				</dl>
			{/if}
			{#if isQueued}
				<p class="source-panel__note">Waiting in the peer's queue</p>
			{/if}
			{#if fallbackLine}
				<p class="source-panel__note source-panel__note--warn">
					<Clock3 class="size-3" aria-hidden="true" />
					{fallbackLine}
				</p>
			{/if}
			{@render tryNext()}
		</div>
	{/if}
	<!-- Plugin sources (source "plugin:<name>") intentionally take the generic path:
	no remote-queue hints below - only soulseek peers report queue telemetry. -->
{:else if source === 'soulseek' && task.status === 'downloading' && !task.held_for_review}
	<div
		class:source-telemetry-compact={compact}
		class="source-telemetry mt-2"
		role="status"
		aria-live="polite"
		aria-atomic="true"
	>
		<div class="min-w-0">
			{#if quality}
				<p class="source-quality">{quality}</p>
			{/if}
			<p class="mt-0.5 text-xs text-muted">
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
			{#if fallbackLine}
				<span class="text-warning">
					<Clock3 class="size-3" aria-hidden="true" />
					{fallbackLine}
				</span>
			{/if}
			{#each evidenceChips as chip (chip)}
				<span>{chip}</span>
			{/each}
		</div>

		{#if canTryNext || actionMessage}
			<div class="source-next">{@render tryNext()}</div>
		{/if}
	</div>
{/if}

<style>
	.source-panel {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.375rem;
		min-width: 0;
	}
	.source-panel__label {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.24em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.source-panel__name {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 1.0625rem;
		font-weight: 700;
	}
	.source-panel__facts {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: 0.25rem 1.5rem;
		margin-top: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
	}
	.source-panel__facts dt {
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	.source-panel__facts dd {
		min-width: 0;
		overflow-wrap: anywhere;
	}
	.source-panel__note {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		font-size: 0.75rem;
		color: var(--dn-muted);
	}
	.source-panel__note--warn {
		color: var(--color-warning);
	}
	.source-telemetry {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.5rem 1rem;
		max-width: 42rem;
		padding: 0.65rem 0.75rem;
		border-left: 2px solid var(--dn-title);
		border-radius: 0.25rem 0.75rem 0.75rem 0.25rem;
		background: var(--color-base-300);
	}
	.source-quality {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
	.source-facts {
		display: flex;
		grid-column: 1 / -1;
		flex-wrap: wrap;
		gap: 0.35rem 0.85rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		color: var(--dn-muted);
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
