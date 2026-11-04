<script lang="ts">
	import { derivedDownloadStatus } from '$lib/features/downloads/queries/downloadStatus';
	import { downloadStatusConfig } from '$lib/features/downloads/queries/downloadStatusConfig';
	import type { DownloadTask } from '$lib/features/downloads/queries/types';

	import VinylProgress from './VinylProgress.svelte';

	// a single track's in-flight download - the tiny pressing vinyl on its row, poll-driven (the
	// album page refreshes the task list while anything is active, so no per-row SSE needed)
	let { task }: { task: DownloadTask } = $props();

	const derivedStatus = $derived(derivedDownloadStatus(task));
	const isMoving = $derived(task.status === 'downloading' || task.status === 'processing');
	const label = $derived(
		isMoving
			? `${downloadStatusConfig[derivedStatus].label} ${task.progress_percent}%`
			: downloadStatusConfig[derivedStatus].label
	);
</script>

<span class="inline-flex shrink-0" title={label} aria-label="Track download: {label}">
	<VinylProgress percent={task.progress_percent} indeterminate={!isMoving} spinning size={22} />
</span>
