<script lang="ts">
	import type { DownloadTask } from '$lib/features/downloads/queries/types';

	interface Props {
		task: DownloadTask;
	}

	let { task }: Props = $props();

	// One reason per task: what happened and what to do.
	const reason = $derived(task.reason ?? null);
	// A queued task waiting for a download source is not a failure: it starts by itself.
	const waiting = $derived(reason?.code === 'no_download_source');
	// A settled reason (nothing better found, stopped) is a final word, not a fault.
	const tone = $derived(
		reason?.kind === 'settled'
			? 'text-muted'
			: waiting || task.held_for_review || task.status === 'partial'
				? 'text-warning/85'
				: 'text-error/80'
	);

	// "7 of 12 imported, 2 held" when the import got part of the way.
	const tally = $derived.by(() => {
		const decision = task.decision;
		if (!decision || decision.files_total === 0) return null;
		if (decision.files_imported === 0 && decision.files_held === 0) return null;
		const parts = [`${decision.files_imported} of ${decision.files_total} imported`];
		if (decision.files_held > 0) parts.push(`${decision.files_held} held`);
		return parts.join(', ');
	});
	// What a clean import did beside placing the files.
	const importNote = $derived(
		task.decision?.outcome === 'imported' ? (task.decision.detail ?? null) : null
	);
</script>

{#if reason}
	<div class="mt-1 text-xs" data-reason-code={reason.code}>
		<p class="line-clamp-2 {tone}">{reason.text}</p>
		<p class="line-clamp-2 text-muted">{reason.action}</p>
		{#if tally}
			<p class="text-muted">{tally}</p>
		{/if}
	</div>
{:else if importNote}
	<p class="mt-1 text-xs text-muted">{importNote}</p>
{/if}
