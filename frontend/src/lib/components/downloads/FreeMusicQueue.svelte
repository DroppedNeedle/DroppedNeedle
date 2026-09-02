<script lang="ts">
	import { CheckCircle2, CircleAlert, Loader2, RotateCw, Search, Trash2, X } from 'lucide-svelte';
	import {
		cancelFreeMusicMutation,
		clearFreeMusicHistoryMutation,
		removeFreeMusicHistoryMutation,
		retryFreeMusicMutation
	} from '$lib/queries/free-music/FreeMusicMutations.svelte';
	import { getFreeMusicTasksQuery } from '$lib/queries/free-music/FreeMusicQueries.svelte';
	import type { FreeMusicStatus, FreeMusicTask } from '$lib/queries/free-music/types';
	import { authStore } from '$lib/stores/authStore.svelte';

	interface Props {
		showAll?: boolean;
	}
	let { showAll = false }: Props = $props();

	const tasksQuery = getFreeMusicTasksQuery(
		() => true,
		() => showAll && authStore.isAdmin
	);
	const cancel = cancelFreeMusicMutation();
	const retry = retryFreeMusicMutation();
	const removeHistory = removeFreeMusicHistoryMutation();
	const clearHistory = clearFreeMusicHistoryMutation();
	const tasks = $derived(tasksQuery.data?.tasks ?? []);

	const labels: Record<FreeMusicStatus, string> = {
		searching: 'Searching the Archive…',
		downloading: 'Downloading',
		importing: 'Importing',
		completed: 'In your library',
		failed: 'Failed',
		cancelled: 'Cancelled'
	};

	const isActive = (status: FreeMusicStatus) =>
		status === 'searching' || status === 'downloading' || status === 'importing';
	const isCancellable = (status: FreeMusicStatus) =>
		status === 'searching' || status === 'downloading';
	const isTerminal = (status: FreeMusicStatus) =>
		status === 'completed' || status === 'failed' || status === 'cancelled';
	const terminalCount = $derived(tasks.filter((task) => isTerminal(task.status)).length);

	function percent(task: FreeMusicTask): number {
		if (task.bytes_total > 0) {
			return Math.min(100, Math.round((task.bytes_downloaded / task.bytes_total) * 100));
		}
		if (task.files_total > 0) {
			return Math.round((task.files_completed / task.files_total) * 100);
		}
		return 0;
	}

	// "CC BY-NC-SA 3.0" from the licence URL. Users should see what they're getting.
	function licenceLabel(url: string): string {
		const match = /creativecommons\.org\/(licenses|publicdomain)\/([^/]+)\/([^/]+)/.exec(url);
		if (!match) return '';
		if (match[1] === 'publicdomain') return `Public domain (${match[2].toUpperCase()})`;
		return `CC ${match[2].toUpperCase()} ${match[3]}`;
	}
</script>

{#if tasks.length}
	<div class="mb-6 space-y-2">
		<div class="flex min-h-7 items-center justify-between gap-3">
			<h2 class="text-sm font-semibold text-base-content/70">Free Music</h2>
			{#if terminalCount > 0}
				<button
					class="btn btn-ghost btn-xs gap-1 text-base-content/60 hover:text-error"
					onclick={() => clearHistory.mutate(showAll && authStore.isAdmin)}
					disabled={clearHistory.isPending || removeHistory.isPending}
					title="Remove finished Free Music entries. Library files stay in place."
				>
					<Trash2 class="h-3.5 w-3.5" aria-hidden="true" /> Clear history
				</button>
			{/if}
		</div>
		{#each tasks as task (task.id)}
			<div class="rounded-2xl border border-base-content/10 bg-base-200/40 px-4 py-3">
				<div class="flex flex-wrap items-center justify-between gap-2">
					<div class="min-w-0 flex-1">
						<div class="flex min-w-0 items-center gap-2">
							{#if task.status === 'completed'}
								<CheckCircle2 class="h-4 w-4 shrink-0 text-success" aria-hidden="true" />
							{:else if task.status === 'failed'}
								<CircleAlert class="h-4 w-4 shrink-0 text-error" aria-hidden="true" />
							{:else if task.status === 'searching'}
								<Search class="h-4 w-4 shrink-0 animate-pulse text-info" aria-hidden="true" />
							{:else if isActive(task.status)}
								<Loader2 class="h-4 w-4 shrink-0 animate-spin text-info" aria-hidden="true" />
							{:else}
								<X class="h-4 w-4 shrink-0 text-base-content/40" aria-hidden="true" />
							{/if}
							<p class="truncate text-sm font-medium">
								{task.artist ? `${task.artist} - ` : ''}{task.title}
							</p>
							{#if task.licence_url}
								{@const licence = licenceLabel(task.licence_url)}
								{#if licence}
									<a
										href={task.licence_url}
										target="_blank"
										rel="noopener noreferrer"
										class="badge badge-ghost badge-sm shrink-0 hover:badge-outline"
										title="This music is licensed for downloading"
									>
										{licence}
									</a>
								{/if}
							{/if}
						</div>
						<p class="mt-0.5 text-xs text-base-content/50">
							{labels[task.status]}
							{#if task.status === 'downloading' && task.files_total}
								· {task.files_completed}/{task.files_total} files
							{/if}
							{#if task.format}
								· {task.format.toUpperCase()}
							{/if}
						</p>
						{#if task.quality_snapshot_summary}
							<p
								class="mt-1 max-w-full truncate text-[11px] text-base-content/45"
								data-testid="free-music-quality-summary"
								title={task.quality_snapshot_summary}
							>
								{task.quality_snapshot_summary}
							</p>
						{/if}
						{#if task.error}
							<p class="mt-0.5 truncate text-xs text-error" title={task.error}>{task.error}</p>
						{/if}
					</div>

					<div class="flex shrink-0 items-center gap-1">
						{#if isCancellable(task.status)}
							<button
								class="btn btn-ghost btn-xs"
								onclick={() => cancel.mutate(task.id)}
								disabled={cancel.isPending}
								aria-label="Cancel {task.title}"
							>
								<X class="h-3.5 w-3.5" aria-hidden="true" />
							</button>
						{:else if task.status === 'failed' || task.status === 'cancelled'}
							<button
								class="btn btn-ghost btn-xs gap-1"
								onclick={() => retry.mutate(task.id)}
								disabled={retry.isPending || removeHistory.isPending}
							>
								<RotateCw class="h-3.5 w-3.5" aria-hidden="true" />
								Retry
							</button>
						{/if}
						{#if isTerminal(task.status)}
							<button
								class="btn btn-ghost btn-xs text-base-content/50 hover:text-error"
								onclick={() => removeHistory.mutate(task.id)}
								disabled={removeHistory.isPending || retry.isPending || clearHistory.isPending}
								aria-label="Remove {task.title} from history"
								title="Remove this history entry. Library files stay in place."
							>
								<Trash2 class="h-3.5 w-3.5" aria-hidden="true" />
							</button>
						{/if}
					</div>
				</div>

				{#if isActive(task.status)}
					<progress
						class="progress progress-primary mt-2 h-1 w-full"
						value={percent(task)}
						max="100"
					></progress>
				{/if}
			</div>
		{/each}
	</div>
{/if}
