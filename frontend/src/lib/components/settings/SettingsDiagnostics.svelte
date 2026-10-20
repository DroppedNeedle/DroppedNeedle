<script lang="ts">
	import { Activity, ArrowUpRight, Gauge, ListChecks } from 'lucide-svelte';

	import {
		getProviderStatsQuery,
		getQueueStatsQuery
	} from '$lib/queries/diagnostics/DiagnosticsQueries.svelte';
	import { formatBytes } from '$lib/utils/formatting';

	import {
		formatCount,
		groupCallRows,
		humanizeWireValue,
		isDiagnosticsPollingEnabled,
		providerLabel
	} from './diagnosticsDisplay';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';

	// The section mounts only while its settings tab is active; document
	// visibility is mirrored into state so both query `enabled` flags flip
	// without a remount and a hidden window stops issuing requests.
	let sectionVisible = $state(
		typeof document === 'undefined' || document.visibilityState === 'visible'
	);

	function handleVisibilityChange(): void {
		sectionVisible = document.visibilityState === 'visible';
	}

	const queueQuery = getQueueStatsQuery(() => isDiagnosticsPollingEnabled(sectionVisible));
	const providerQuery = getProviderStatsQuery(() => isDiagnosticsPollingEnabled(sectionVisible));

	const jobs = $derived(queueQuery.data?.jobs ?? []);
	const queues = $derived(queueQuery.data?.queues ?? []);

	function heartbeatText(at: number | null | undefined): string {
		if (!at) return 'never';
		return new Date(at).toLocaleTimeString();
	}
</script>

<svelte:window onvisibilitychange={handleVisibilityChange} />

<Card>
	{#snippet header()}
		<div class="flex items-center gap-2">
			<Activity class="h-5 w-5 text-primary" aria-hidden="true" />
			<CardTitle>Diagnostics</CardTitle>
		</div>
	{/snippet}
	<p class="text-sm text-base-content/60">
		Live numbers from the server. Counts reset when it restarts. Refreshes every five seconds while
		open.
	</p>

	<QueryBoundary query={providerQuery} what="provider stats">
		{#snippet loading()}
			<div
				class="skeleton h-40 w-full rounded-2xl"
				aria-busy="true"
				aria-label="Loading provider stats"
			></div>
		{/snippet}
		{#snippet children(stats)}
			{@const callGroups = groupCallRows(stats.calls)}
			{@const limiters = [...stats.limiters].sort((a, b) =>
				providerLabel(a.source).localeCompare(providerLabel(b.source))
			)}
			<section class="mt-2 space-y-3" aria-label="Outbound provider calls">
				<div class="flex items-center gap-2">
					<ArrowUpRight class="h-4 w-4 text-base-content/60" aria-hidden="true" />
					<h3 class="font-semibold">Outbound provider calls</h3>
				</div>
				{#if callGroups.length === 0}
					<p class="text-sm text-base-content/50">No calls since the server started.</p>
				{:else}
					<div class="overflow-x-auto">
						<table class="table table-sm max-sm:[&_td]:px-2 max-sm:[&_th]:px-2">
							<thead>
								<tr>
									<th scope="col">Priority</th>
									<th scope="col">Outcome</th>
									<th scope="col" class="text-right">Calls</th>
									<th scope="col" class="text-right">Per min</th>
									<th scope="col" class="text-right">Body</th>
								</tr>
							</thead>
							{#each callGroups as group (group.source)}
								<tbody>
									<tr class="bg-base-100/60">
										<th scope="rowgroup" colspan="5">
											{group.label}
											<span class="font-normal text-base-content/60">
												{formatCount(group.totalCalls)} calls
											</span>
										</th>
									</tr>
									{#each group.rows as row (row.key)}
										<tr>
											<td class="whitespace-nowrap">{row.priority}</td>
											<td class="whitespace-nowrap">{row.outcome}</td>
											<td class="text-right tabular-nums">{formatCount(row.calls)}</td>
											<td class="text-right tabular-nums">{formatCount(row.lastMinute)}</td>
											<td class="text-right whitespace-nowrap tabular-nums">
												{formatBytes(row.bodyBytes)}
											</td>
										</tr>
									{/each}
								</tbody>
							{/each}
						</table>
					</div>
					<p class="text-xs text-base-content/60">Retries count as calls. Body excludes headers.</p>
				{/if}
			</section>

			<section class="space-y-3" aria-label="Rate limits">
				<div class="flex items-center gap-2">
					<Gauge class="h-4 w-4 text-base-content/60" aria-hidden="true" />
					<h3 class="font-semibold">Rate limits</h3>
				</div>
				<div class="overflow-x-auto">
					<table class="table table-sm max-sm:[&_td]:px-2 max-sm:[&_th]:px-2">
						<thead>
							<tr>
								<th scope="col">Provider</th>
								<th scope="col" class="text-right">Per sec</th>
								<th scope="col" class="text-right">Burst</th>
								<th scope="col" class="text-right">Tokens left</th>
							</tr>
						</thead>
						<tbody>
							{#each limiters as limiter (limiter.source)}
								<tr>
									<td>{providerLabel(limiter.source)}</td>
									<td class="text-right tabular-nums">{limiter.per_second}</td>
									<td class="text-right tabular-nums">{formatCount(limiter.burst)}</td>
									<td class="text-right tabular-nums">{formatCount(limiter.remaining)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			</section>
		{/snippet}
	</QueryBoundary>

	<section class="space-y-3" aria-label="Background jobs">
		<div class="flex items-center gap-2">
			<ListChecks class="h-4 w-4 text-base-content/60" aria-hidden="true" />
			<h3 class="font-semibold">Background jobs</h3>
		</div>
		{#if queueQuery.isLoading}
			<div
				class="skeleton h-40 w-full rounded-2xl"
				aria-busy="true"
				aria-label="Loading background jobs"
			></div>
		{:else if queueQuery.error}
			<div class="alert alert-error" role="alert">
				<span>Couldn't load queue stats.</span>
			</div>
		{:else if jobs.length === 0}
			<p class="text-sm text-base-content/50">No background jobs registered.</p>
		{:else}
			<div class="overflow-x-auto">
				<table class="table table-sm">
					<thead>
						<tr>
							<th scope="col">Job</th>
							<th scope="col">State</th>
							<th scope="col">Last heartbeat</th>
							<th scope="col">Last error</th>
						</tr>
					</thead>
					<tbody>
						{#each jobs as job (job.name)}
							<tr>
								<td>{humanizeWireValue(job.name)}</td>
								<td>{humanizeWireValue(job.state)}</td>
								<td>{heartbeatText(job.last_heartbeat_at)}</td>
								<td>{job.last_error ?? 'none'}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</section>

	{#if queueQuery.data}
		<section class="space-y-3" aria-label="Job queues">
			<h3 class="font-semibold">Job queues</h3>
			{#if queues.length === 0}
				<p class="text-sm text-base-content/50">No queued work.</p>
			{:else}
				<div class="overflow-x-auto">
					<table class="table table-sm">
						<thead>
							<tr>
								<th scope="col">Kind</th>
								<th scope="col">Queued</th>
								<th scope="col">Running</th>
								<th scope="col">Failed</th>
							</tr>
						</thead>
						<tbody>
							{#each queues as queue (queue.kind)}
								<tr>
									<td>{queue.kind}</td>
									<td>{formatCount(queue.queued)}</td>
									<td>{formatCount(queue.running)}</td>
									<td>{formatCount(queue.failed)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		</section>
	{/if}
</Card>
