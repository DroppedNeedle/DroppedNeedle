<script lang="ts">
	import type { DownloadTask } from '$lib/types';

	interface Props {
		task: DownloadTask;
	}

	let { task }: Props = $props();

	// One reason per task: what happened and what to do. Older servers sent only the
	// raw error_message, so that stays as the fallback.
	const reason = $derived(task.reason ?? null);
	const fallback = $derived(reason ? null : task.error_message);
	const tone = $derived(
		task.held_for_review || task.status === 'partial' ? 'text-warning/85' : 'text-error/80'
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
</script>

{#if reason}
	<div class="mt-1 text-xs" data-reason-code={reason.code}>
		<p class="line-clamp-2 {tone}">{reason.text}</p>
		<p class="line-clamp-2 text-base-content/60">{reason.action}</p>
		{#if tally}
			<p class="text-base-content/50">{tally}</p>
		{/if}
	</div>
{:else if fallback}
	<p class="mt-1 line-clamp-2 text-xs {tone}">{fallback}</p>
{/if}
