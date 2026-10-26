<script lang="ts">
	import HubShelf from '$lib/components/HubShelf.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { reveal } from '$lib/actions/reveal';
	import { getRemoteHistoryQuery } from '$lib/queries/remotes/RemoteQueries.svelte';
	import type { RemoteSourceDescriptor } from '$lib/queries/remotes/remoteSources';
	import { withBasePath } from '$lib/utils/basePath';
	import { formatViewedAt } from './history';

	const SHOWN = 10;

	let { remote }: { remote: RemoteSourceDescriptor } = $props();

	const historyQuery = getRemoteHistoryQuery(
		() => remote.source,
		() => ({ limit: SHOWN })
	);
	const entries = $derived(historyQuery.data?.items ?? []);
	const total = $derived(historyQuery.data?.total ?? 0);
</script>

<div use:reveal>
	<HubShelf title="Listening History" loading={historyQuery.isFetching}>
		{#if entries.length > 0}
			<div class="overflow-x-auto rounded-lg">
				<table class="table table-sm">
					<thead>
						<tr>
							<th>Track</th>
							<th>Artist</th>
							<th>Album</th>
							<th>When</th>
						</tr>
					</thead>
					<tbody>
						{#each entries as entry (entry.id + entry.viewed_at)}
							<tr
								class="hover transition-all duration-200 hover:border-l-2 hover:border-l-primary hover:pl-1"
							>
								<td class="font-medium">{entry.track_title}</td>
								<td class="text-base-content/60">{entry.artist_name}</td>
								<td class="text-base-content/60">{entry.album_name}</td>
								<td class="text-base-content/50 text-xs">{formatViewedAt(entry.viewed_at)}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
			{#if total > SHOWN}
				<div class="mt-2 flex gap-2">
					<Button
						variant="outline"
						size="sm"
						href={withBasePath(`/library/${remote.source}/activity`)}
					>
						View full history and analytics ({total.toLocaleString()} plays)
					</Button>
				</div>
			{/if}
		{:else if historyQuery.isError}
			<p class="text-sm text-base-content/50">Couldn't load the listening history.</p>
		{:else if !historyQuery.isFetching}
			<p class="text-sm text-base-content/50">No listening history is available yet.</p>
		{/if}
	</HubShelf>
</div>
