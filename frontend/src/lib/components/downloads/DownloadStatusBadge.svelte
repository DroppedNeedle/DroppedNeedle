<script lang="ts">
	import {
		derivedDownloadStatus,
		formatRetryEta,
		retryDisplay
	} from '$lib/queries/downloads/downloadStatus';
	import {
		downloadStatusConfig,
		retryBadgeConfig
	} from '$lib/queries/downloads/downloadStatusConfig';
	import type { DownloadTask } from '$lib/types';

	let { task }: { task: DownloadTask } = $props();

	// A failed/partial task that auto-retries shows the retry treatment; otherwise the
	// normal status. (retryDisplay reads next_retry_at/retry_max from the backend.)
	const retry = $derived(retryDisplay(task));
	const derivedStatus = $derived(derivedDownloadStatus(task));
	const cfg = $derived(retry ? retryBadgeConfig[retry.kind] : downloadStatusConfig[derivedStatus]);
	const label = $derived.by(() => {
		if (retry?.kind === 'retrying') return `Retrying (attempt ${retry.attempt}/${retry.max})`;
		if (retry?.kind === 'scheduled') return `Retry scheduled · ${formatRetryEta(retry.etaMinutes)}`;
		if (retry?.kind === 'failed_exhausted') return 'Failed · out of retries';
		if (derivedStatus === 'partial')
			return `Partial - ${task.files_completed}/${task.files_total} tracks`;
		return downloadStatusConfig[derivedStatus].label;
	});
</script>

<span
	class="badge {cfg.badgeClass} badge-sm gap-1 {cfg.pulse ? 'animate-pulse' : ''}"
	aria-label="Status: {label}"
>
	<cfg.icon class="h-3 w-3" aria-hidden="true" />
	{label}
</span>
