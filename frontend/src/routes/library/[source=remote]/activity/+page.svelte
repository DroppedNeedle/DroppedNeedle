<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import {
		getRemoteAnalyticsQuery,
		getRemoteHistoryQuery
	} from '$lib/queries/remotes/RemoteQueries.svelte';
	import { ChevronLeft } from 'lucide-svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { formatViewedAt } from '../history';
	import type { PageData } from './$types';

	const PAGE_SIZE = 50;

	let { data }: { data: PageData } = $props();
	const remote = $derived(data.remote);

	let historyOffset = $state(0);

	const analyticsQuery = getRemoteAnalyticsQuery(() => remote.source);
	const historyQuery = getRemoteHistoryQuery(
		() => remote.source,
		() => ({ limit: PAGE_SIZE, offset: historyOffset })
	);

	const historyTotal = $derived(historyQuery.data?.total ?? 0);
	const currentPage = $derived(Math.floor(historyOffset / PAGE_SIZE) + 1);
	const totalPages = $derived(Math.ceil(historyTotal / PAGE_SIZE));
</script>

<div class="container mx-auto space-y-6 p-6">
	<div class="flex items-center gap-3">
		<Button
			variant="ghost"
			size="sm"
			icon={ChevronLeft}
			href={withBasePath(`/library/${remote.source}`)}
		>
			Back
		</Button>
		<remote.icon class="h-6 w-6 {remote.tone}" />
		<h1 class="text-2xl font-bold">{remote.label} activity and analytics</h1>
	</div>

	<QueryBoundary query={analyticsQuery} what="{remote.label} analytics">
		{#snippet loading()}
			<div class="flex justify-center py-8" role="status" aria-label="Loading analytics">
				<span class="loading loading-spinner loading-lg" aria-hidden="true"></span>
			</div>
		{/snippet}
		{#snippet children(analytics)}
			<div class="grid grid-cols-2 sm:grid-cols-4 gap-4">
				<div class="stat bg-base-200 rounded-lg p-4">
					<div class="stat-title text-xs">Total Listens</div>
					<div class="stat-value text-2xl">{analytics.total_listens.toLocaleString()}</div>
				</div>
				<div class="stat bg-base-200 rounded-lg p-4">
					<div class="stat-title text-xs">Last 7 Days</div>
					<div class="stat-value text-2xl">{analytics.listens_last_7_days.toLocaleString()}</div>
				</div>
				<div class="stat bg-base-200 rounded-lg p-4">
					<div class="stat-title text-xs">Last 30 Days</div>
					<div class="stat-value text-2xl">{analytics.listens_last_30_days.toLocaleString()}</div>
				</div>
				<div class="stat bg-base-200 rounded-lg p-4">
					<div class="stat-title text-xs">Hours Listened</div>
					<div class="stat-value text-2xl">{analytics.total_hours}</div>
				</div>
			</div>

			{#if !analytics.is_complete}
				<div class="alert alert-info alert-soft text-sm">
					These stats are based on {analytics.entries_analyzed.toLocaleString()} of {analytics.total_listens.toLocaleString()}
					total plays.
				</div>
			{/if}

			<div class="grid grid-cols-1 md:grid-cols-3 gap-6">
				{#each [{ title: 'Top Artists', items: analytics.top_artists, subtitles: false }, { title: 'Top Albums', items: analytics.top_albums, subtitles: true }, { title: 'Top Tracks', items: analytics.top_tracks, subtitles: true }] as group (group.title)}
					{#if group.items.length > 0}
						<div>
							<h3 class="text-lg font-semibold mb-3">{group.title}</h3>
							<div class="space-y-2">
								{#each group.items as item, i (item.name)}
									<div class="flex items-center gap-3 p-2 rounded-lg hover:bg-base-200">
										<span class="text-base-content/40 w-5 text-right font-mono text-sm"
											>{i + 1}</span
										>
										<div class="flex-1 min-w-0">
											<p class="font-medium truncate">{item.name}</p>
											{#if group.subtitles}
												<p class="text-xs text-base-content/60 truncate">{item.subtitle}</p>
											{/if}
										</div>
										<span class="badge badge-sm">{item.play_count}</span>
									</div>
								{/each}
							</div>
						</div>
					{/if}
				{/each}
			</div>
		{/snippet}
	</QueryBoundary>

	<div>
		<h2 class="text-xl font-bold mb-4">Full listening history</h2>
		<QueryBoundary query={historyQuery} what="Listening history">
			{#snippet loading()}
				<div class="flex justify-center py-4" role="status" aria-label="Loading history">
					<span class="loading loading-spinner loading-md" aria-hidden="true"></span>
				</div>
			{/snippet}
			{#snippet empty()}
				<p class="text-sm text-base-content/50">No listening history is available yet.</p>
			{/snippet}
			{#snippet children(history)}
				<div class="overflow-x-auto rounded-lg">
					<table class="table table-sm">
						<thead>
							<tr>
								<th>Track</th>
								<th>Artist</th>
								<th>Album</th>
								<th>When</th>
								<th>Device</th>
							</tr>
						</thead>
						<tbody>
							{#each history.items as entry (entry.id + entry.viewed_at)}
								<tr class="hover">
									<td class="font-medium">{entry.track_title}</td>
									<td class="text-base-content/60">{entry.artist_name}</td>
									<td class="text-base-content/60">{entry.album_name}</td>
									<td class="text-base-content/50 text-xs">{formatViewedAt(entry.viewed_at)}</td>
									<td class="text-base-content/50 text-xs">{entry.device_name}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>

				<div class="flex items-center justify-between mt-4">
					<Button
						variant="outline"
						size="sm"
						disabled={historyOffset === 0}
						onclick={() => (historyOffset = Math.max(0, historyOffset - PAGE_SIZE))}
						>Previous</Button
					>
					<span class="text-sm text-base-content/60">
						Page {currentPage} of {totalPages} ({historyTotal.toLocaleString()} total plays)
					</span>
					<Button
						variant="outline"
						size="sm"
						disabled={historyOffset + PAGE_SIZE >= historyTotal}
						onclick={() => (historyOffset += PAGE_SIZE)}>Next</Button
					>
				</div>
			{/snippet}
		</QueryBoundary>
	</div>
</div>
