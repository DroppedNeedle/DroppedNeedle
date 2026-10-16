<script lang="ts">
	import { TriangleAlert, ShieldCheck, Trash2 } from 'lucide-svelte';

	import EmptyState from '$lib/components/EmptyState.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import {
		deleteQuarantineEntry,
		getQuarantineQuery
	} from '$lib/queries/downloads/QuarantineQueries.svelte';

	const query = getQuarantineQuery();
	const del = deleteQuarantineEntry();

	// two-step inline confirm: the deletion is permanent
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
			<Card variant="bordered" padding="sm">
				<div class="flex items-center gap-3">
					<TriangleAlert class="h-5 w-5 shrink-0 text-warning" aria-hidden="true" />
					<div class="min-w-0 flex-1">
						<p class="truncate text-sm font-medium" title={entry.filename}>
							{basename(entry.filename)}
						</p>
						<p class="truncate text-xs text-base-content/55">
							{entry.username}
							<span class="text-base-content/30"> · </span>{entry.reason_text ?? entry.reason}
							<span class="text-base-content/30"> · </span>{fmtDate(entry.quarantined_at)}
						</p>
					</div>
					<Button
						variant={confirmId === entry.id ? 'danger' : 'ghost'}
						size="xs"
						icon={Trash2}
						class="gap-1"
						onclick={() => requestDelete(entry.id)}
						disabled={del.isPending}
					>
						{confirmId === entry.id ? 'Confirm - delete & allow retry' : 'Delete & allow retry'}
					</Button>
				</div>
			</Card>
		{/each}
	</div>
{/if}
