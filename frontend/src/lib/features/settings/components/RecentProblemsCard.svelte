<!--
	Recent problems: the server's latest warnings and errors, newest first.
	A ref a person was given finds the one their request met.
-->
<script lang="ts">
	import { getRecentProblemsQuery } from '$lib/features/settings/queries/diagnostics.svelte';
	import type { components } from '$lib/api/v3/openapi';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	type RecentProblems = components['schemas']['RecentProblemsResponse'];

	let reference = $state('');
	// The search follows the box once typing pauses, not on each key.
	let searched = $state('');
	$effect(() => {
		const typed = reference;
		const timer = setTimeout(() => (searched = typed), 300);
		return () => clearTimeout(timer);
	});
	const query = getRecentProblemsQuery(() => searched);

	function when(ms: number): string {
		return new Date(ms).toLocaleString();
	}
</script>

<Card variant="bordered" padding="md" title="Recent problems">
	<div class="space-y-4">
		<TextField label="Ref" type="search" placeholder="4f1c2a9e" mono bind:value={reference} />
		<QueryBoundary
			{query}
			what="recent problems"
			isEmpty={(data: RecentProblems) => data.problems.length === 0}
		>
			{#snippet empty()}
				<p class="text-sm text-muted">
					{searched.trim() ? 'Nothing with that ref.' : 'No warnings or errors since the start.'}
				</p>
			{/snippet}
			{#snippet children(data: RecentProblems)}
				<ul class="divide-y divide-line">
					{#each data.problems as problem, index (index)}
						<li class="flex flex-wrap items-start gap-3 py-3">
							<span class="w-40 shrink-0 text-sm text-muted">{when(problem.last_at)}</span>
							<StatusChip tone={problem.level === 'error' ? 'error' : 'warning'}>
								{problem.level === 'error' ? 'Error' : 'Warning'}
							</StatusChip>
							<span class="min-w-0 flex-1 break-words">{problem.message}</span>
							{#if problem.location}
								<span class="font-mono text-xs text-muted">{problem.location}</span>
							{/if}
							{#if problem.count > 1}
								<span class="text-sm text-muted">×{problem.count}</span>
							{/if}
						</li>
					{/each}
				</ul>
			{/snippet}
		</QueryBoundary>
	</div>
</Card>
