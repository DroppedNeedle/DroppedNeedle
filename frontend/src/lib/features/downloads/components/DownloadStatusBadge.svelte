<script lang="ts">
	import {
		derivedDownloadStatus,
		formatRetryEta,
		isSettled,
		retryDisplay
	} from '$lib/features/downloads/queries/downloadStatus';
	import {
		downloadStatusConfig,
		retryBadgeConfig,
		settledBadgeConfig
	} from '$lib/features/downloads/queries/downloadStatusConfig';
	import type { DownloadTask } from '$lib/features/downloads/queries/types';
	import StatusChip from '$lib/ui/StatusChip.svelte';

	let { task }: { task: DownloadTask } = $props();

	// A failed/partial task that auto-retries shows the retry treatment; otherwise the
	// normal status. (retryDisplay reads next_retry_at/retry_max from the backend.)
	const retry = $derived(retryDisplay(task));
	const derivedStatus = $derived(derivedDownloadStatus(task));
	const settled = $derived(isSettled(task));
	const cfg = $derived(
		retry
			? retryBadgeConfig[retry.kind]
			: settled
				? settledBadgeConfig
				: downloadStatusConfig[derivedStatus]
	);
	const label = $derived.by(() => {
		if (retry?.kind === 'retrying') return `Retrying (attempt ${retry.attempt}/${retry.max})`;
		if (retry?.kind === 'scheduled') return `Retry scheduled · ${formatRetryEta(retry.etaMinutes)}`;
		if (retry?.kind === 'failed_exhausted') return 'Failed · out of retries';
		if (derivedStatus === 'partial')
			return `Partial - ${task.files_completed}/${task.files_total} tracks`;
		if (settled) return settledBadgeConfig.label;
		return downloadStatusConfig[derivedStatus].label;
	});

	type Tone = 'action' | 'neutral' | 'info' | 'success' | 'warning' | 'error';
	// Acid only for the live transfer; amber for waiting and retries, red for failure.
	const tone = $derived.by((): Tone => {
		if (retry) return retry.kind === 'failed_exhausted' ? 'error' : 'warning';
		if (settled) return 'neutral';
		switch (derivedStatus) {
			case 'downloading':
				return 'action';
			case 'processing':
				return 'info';
			case 'completed':
				return 'success';
			case 'awaiting_review':
			case 'partial':
				return 'warning';
			case 'failed':
				return 'error';
			default:
				return 'neutral';
		}
	});
</script>

<span class="inline-flex" aria-label="Status: {label}">
	<StatusChip {tone} icon={cfg.icon}>{label}</StatusChip>
</span>
