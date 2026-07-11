<script lang="ts">
	import { resolve } from '$app/paths';
	import { API } from '$lib/constants';
	import { api } from '$lib/api/client';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import type { PlexAnalyticsResponse, PlexHistoryResponse, PlexHistoryEntry } from '$lib/types';

	let analytics = $state<PlexAnalyticsResponse | null>(null);
	let analyticsLoading = $state(true);

	let history = $state<PlexHistoryEntry[]>([]);
	let historyTotal = $state(0);
	let historyOffset = $state(0);
	let historyLoading = $state(false);
	const PAGE_SIZE = 50;

	async function loadAnalytics() {
		analyticsLoading = true;
		try {
			analytics = await api.get<PlexAnalyticsResponse>(API.plexLibrary.analytics());
		} catch {
			analytics = null;
		} finally {
			analyticsLoading = false;
		}
	}

	async function loadHistory(offset = 0) {
		historyLoading = true;
		try {
			const resp = await api.get<PlexHistoryResponse>(API.plexLibrary.history(PAGE_SIZE, offset));
			history = resp.entries;
			historyTotal = resp.total;
			historyOffset = offset;
		} catch {
			history = [];
		} finally {
			historyLoading = false;
		}
	}

	function formatViewedAt(ts: string): string {
		try {
			const d = new Date(Number(ts) * 1000);
			return d.toLocaleDateString(undefined, {
				month: 'short',
				day: 'numeric',
				hour: '2-digit',
				minute: '2-digit'
			});
		} catch {
			return '';
		}
	}

	let currentPage = $derived(Math.floor(historyOffset / PAGE_SIZE) + 1);
	let totalPages = $derived(Math.ceil(historyTotal / PAGE_SIZE));

	$effect(() => {
		loadAnalytics();
		loadHistory();
	});
</script>

<div class="container mx-auto space-y-6 p-6">
	<div class="flex items-center gap-3">
		<a href={resolve('/library/plex')} class="btn btn-ghost btn-sm">← Back</a>
		<PlexIcon class="h-6 w-6" style="color: rgb(var(--brand-plex));" />
		<h1 class="text-2xl font-bold">Plex activity and analytics</h1>
	</div>

	{#if analyticsLoading}
		<div class="flex justify-center py-8">
			<span class="loading loading-spinner loading-lg"></span>
		</div>
	{:else if analytics}
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
			{#if analytics.top_artists.length > 0}
				<div>
					<h3 class="text-lg font-semibold mb-3">Top Artists</h3>
					<div class="space-y-2">
						{#each analytics.top_artists as item, i (item.name)}
							<div class="flex items-center gap-3 p-2 rounded-lg hover:bg-base-200">
								<span class="text-base-content/40 w-5 text-right font-mono text-sm">{i + 1}</span>
								<div class="flex-1 min-w-0">
									<p class="font-medium truncate">{item.name}</p>
								</div>
								<span class="badge badge-sm">{item.play_count}</span>
							</div>
						{/each}
					</div>
				</div>
			{/if}

			{#if analytics.top_albums.length > 0}
				<div>
					<h3 class="text-lg font-semibold mb-3">Top Albums</h3>
					<div class="space-y-2">
						{#each analytics.top_albums as item, i (item.name)}
							<div class="flex items-center gap-3 p-2 rounded-lg hover:bg-base-200">
								<span class="text-base-content/40 w-5 text-right font-mono text-sm">{i + 1}</span>
								<div class="flex-1 min-w-0">
									<p class="font-medium truncate">{item.name}</p>
									<p class="text-xs text-base-content/60 truncate">{item.subtitle}</p>
								</div>
								<span class="badge badge-sm">{item.play_count}</span>
							</div>
						{/each}
					</div>
				</div>
			{/if}

			{#if analytics.top_tracks.length > 0}
				<div>
					<h3 class="text-lg font-semibold mb-3">Top Tracks</h3>
					<div class="space-y-2">
						{#each analytics.top_tracks as item, i (item.name)}
							<div class="flex items-center gap-3 p-2 rounded-lg hover:bg-base-200">
								<span class="text-base-content/40 w-5 text-right font-mono text-sm">{i + 1}</span>
								<div class="flex-1 min-w-0">
									<p class="font-medium truncate">{item.name}</p>
									<p class="text-xs text-base-content/60 truncate">{item.subtitle}</p>
								</div>
								<span class="badge badge-sm">{item.play_count}</span>
							</div>
						{/each}
					</div>
				</div>
			{/if}
		</div>
	{/if}

	<div>
		<h2 class="text-xl font-bold mb-4">Full listening history</h2>
		{#if historyLoading}
			<div class="flex justify-center py-4">
				<span class="loading loading-spinner loading-md"></span>
			</div>
		{:else if history.length > 0}
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
						{#each history as entry (entry.rating_key + entry.viewed_at)}
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
				<button
					class="btn btn-sm btn-outline"
					disabled={historyOffset === 0}
					onclick={() => loadHistory(Math.max(0, historyOffset - PAGE_SIZE))}>Previous</button
				>
				<span class="text-sm text-base-content/60">
					Page {currentPage} of {totalPages} ({historyTotal.toLocaleString()} total plays)
				</span>
				<button
					class="btn btn-sm btn-outline"
					disabled={historyOffset + PAGE_SIZE >= historyTotal}
					onclick={() => loadHistory(historyOffset + PAGE_SIZE)}>Next</button
				>
			</div>
		{:else}
			<p class="text-sm text-base-content/50">No listening history is available yet.</p>
		{/if}
	</div>
</div>
