<!--
	The Health tab: what is wrong now, worst first. A count row, then one card
	per severity, each row a chip, one sentence, how long and a Fix link.
	Nothing wrong shows one torn card.
-->
<script lang="ts">
	import SettingsPaneHeader from './SettingsPaneHeader.svelte';
	import {
		getAdminProblemsQuery,
		type Problem,
		type ProblemSeverity,
		type ProblemsResponse
	} from '$lib/features/health/queries/problems.svelte';
	import {
		countBySeverity,
		describeProblem,
		howLong
	} from '$lib/features/health/components/problemText';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import VinylRecord from '$lib/ui/VinylRecord.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import StatRow from '$lib/ui/StatRow.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import RecycleBinFolderAccept from '$lib/features/library-management/components/RecycleBinFolderAccept.svelte';

	const query = getAdminProblemsQuery();

	const SECTIONS: { severity: ProblemSeverity; title: string; chip: string }[] = [
		{ severity: 'broken', title: 'Needs you', chip: 'Broken' },
		{ severity: 'warning', title: 'Worth a look', chip: 'Warning' },
		{ severity: 'info', title: 'For your information', chip: 'Info' }
	];
	const TONES: Record<ProblemSeverity, 'error' | 'warning' | 'info'> = {
		broken: 'error',
		warning: 'warning',
		info: 'info'
	};

	function checkedAgo(checkedAt: number): string {
		const seconds = Math.max(0, Math.round((Date.now() - checkedAt) / 1000));
		return seconds < 90 ? `${seconds} s ago` : `${Math.round(seconds / 60)} min ago`;
	}

	function rowKey(problem: Problem): string {
		return `${problem.kind}:${problem.subject ?? ''}`;
	}
</script>

<div class="space-y-5">
	<SettingsPaneHeader title="Health" description="What is wrong right now, worst first." />
	<QueryBoundary
		{query}
		what="the health check"
		isEmpty={(data: ProblemsResponse) => data.problems.length === 0}
	>
		{#snippet empty()}
			<Card variant="torn" padding="lg">
				<div class="flex flex-col items-center gap-3 text-center">
					<VinylRecord sheen="acid" label class="h-28 w-28" />
					<p class="text-lg font-semibold text-title">Nothing is wrong.</p>
				</div>
			</Card>
		{/snippet}
		{#snippet children(data: ProblemsResponse)}
			{@const counts = countBySeverity(data.problems)}
			<StatRow
				label="Problems by severity"
				stats={[
					{ label: 'Need you', value: counts.broken },
					{ label: 'Worth a look', value: counts.warning },
					{ label: 'For your information', value: counts.info },
					{ label: 'Checked', value: checkedAgo(data.checked_at) }
				]}
			/>
			{#each SECTIONS as section (section.severity)}
				{@const rows = data.problems.filter((p) => p.severity === section.severity)}
				{#if rows.length}
					<Card variant="bordered" padding="md" title={section.title}>
						<ul class="divide-y divide-line">
							{#each rows as problem (rowKey(problem))}
								{@const view = describeProblem(problem)}
								{@const since = howLong(problem.since_at, data.checked_at)}
								<li class="flex flex-wrap items-center gap-3 py-3">
									<StatusChip tone={TONES[problem.severity]}>{section.chip}</StatusChip>
									<span class="min-w-0 flex-1">
										{view.sentence}
										{#if problem.stale}<span class="text-sm text-muted">(last known)</span>{/if}
									</span>
									{#if since}<span class="text-sm text-muted">{since}</span>{/if}
									{#if problem.accept_folder}
										<RecycleBinFolderAccept path={problem.accept_folder} />
									{/if}
									{#if view.fixHref}
										<Button variant="outline" size="xs" href={withBasePath(view.fixHref)}>
											Fix
										</Button>
									{/if}
								</li>
							{/each}
						</ul>
					</Card>
				{/if}
			{/each}
		{/snippet}
	</QueryBoundary>
</div>
