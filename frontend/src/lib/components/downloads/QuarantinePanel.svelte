<script lang="ts">
	import { TriangleAlert, ShieldCheck, Trash2 } from 'lucide-svelte';

	import EmptyState from '$lib/components/EmptyState.svelte';
	import {
		deleteQuarantineEntry,
		getQuarantineQuery
	} from '$lib/queries/downloads/QuarantineQueries.svelte';

	const query = getQuarantineQuery();
	const del = deleteQuarantineEntry();

	// two-step inline confirm; UX-5/§8.9 require confirmation for this permanent deletion
	let confirmId = $state<number | null>(null);

	function requestDelete(id: number) {
		if (confirmId === id) {
			del.mutate(id);
			confirmId = null;
		} else {
			confirmId = id;
			setTimeout(() => {
				if (confirmId === id) confirmId = null;
			}, 3000);
		}
	}

	function fmtDate(ts: number): string {
		return new Date(ts * 1000).toLocaleDateString(undefined, {
			month: 'short',
			day: 'numeric',
			year: 'numeric'
		});
	}

	function basename(path: string): string {
		const parts = path.split(/[\\/]/);
		return parts[parts.length - 1] || path;
	}
</script>

{#if query.isLoading}
	<div class="space-y-2">
		<div class="skeleton h-14 w-full rounded-xl"></div>
		<div class="skeleton h-14 w-full rounded-xl"></div>
	</div>
{:else if query.isError}
	<div class="alert alert-error">Failed to load quarantine entries.</div>
{:else if (query.data?.items.length ?? 0) === 0}
	<EmptyState
		icon={ShieldCheck}
		title="Nothing quarantined"
		description="Files that fail verification are held here. There's nothing to review."
	/>
{:else}
	<div class="space-y-2">
		{#each query.data?.items ?? [] as entry (entry.id)}
			<div
				class="flex items-center gap-3 rounded-xl border border-base-content/5 bg-base-200/50 p-3 backdrop-blur-sm"
			>
				<TriangleAlert class="h-5 w-5 shrink-0 text-warning" aria-hidden="true" />
				<div class="min-w-0 flex-1">
					<p class="truncate text-sm font-medium" title={entry.filename}>
						{basename(entry.filename)}
					</p>
					<p class="truncate text-xs text-base-content/55">
						{entry.username}
						<span class="text-base-content/30"> · </span>{entry.reason}
						<span class="text-base-content/30"> · </span>{fmtDate(entry.quarantined_at)}
					</p>
				</div>
				<button
					class="btn btn-xs gap-1 {confirmId === entry.id ? 'btn-error' : 'btn-ghost'}"
					onclick={() => requestDelete(entry.id)}
					disabled={del.isPending}
				>
					<Trash2 class="h-3.5 w-3.5" />
					{confirmId === entry.id ? 'Confirm - delete & allow retry' : 'Delete & allow retry'}
				</button>
			</div>
		{/each}
	</div>
{/if}
