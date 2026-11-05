<script lang="ts">
	import './hub.css';
	import { ArrowRight, History } from 'lucide-svelte';
	import HubShelf from '$lib/features/remote-library/components/HubShelf.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { reveal } from '$lib/actions/reveal';
	import { getRemoteHistoryQuery } from '$lib/features/remote-library/queries/RemoteQueries.svelte';
	import type { RemoteSourceDescriptor } from '$lib/features/remote-library/queries/remoteSources';
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
	<HubShelf title="Listening History" icon={History} loading={historyQuery.isFetching}>
		{#if entries.length > 0}
			<div class="hist">
				<table class="hub-table">
					<thead>
						<tr>
							<th>Track</th>
							<th class="hist__wide">Artist</th>
							<th class="hist__wide">Album</th>
							<th class="hub-time hist__when">When</th>
						</tr>
					</thead>
					<tbody>
						{#each entries as entry (entry.id + entry.viewed_at)}
							<tr>
								<td class="hub-clip">
									<span class="block truncate font-medium">{entry.track_title}</span>
									<span class="hist__under">{entry.artist_name}</span>
								</td>
								<td class="hub-clip hist__wide">{entry.artist_name}</td>
								<td class="hub-clip hist__wide hub-dim">{entry.album_name}</td>
								<td class="hub-time hist__when">{formatViewedAt(entry.viewed_at)}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
			{#if total > SHOWN}
				<div class="mt-3 flex justify-end px-2 pb-1">
					<Button
						variant="outline"
						size="display-sm"
						icon={ArrowRight}
						iconPosition="end"
						href={withBasePath(`/library/${remote.source}/activity`)}
					>
						View full history and analytics ({total.toLocaleString()} plays)
					</Button>
				</div>
			{/if}
		{:else if historyQuery.isError}
			<p class="text-sm text-muted">Couldn't load the listening history.</p>
		{:else if !historyQuery.isFetching}
			<p class="text-sm text-muted">No listening history is available yet.</p>
		{/if}
	</HubShelf>
</div>

<style>
	.hist {
		container-type: inline-size;
	}
	.hist table {
		table-layout: fixed;
	}
	.hist__when {
		width: 9rem;
		color: var(--dn-muted);
	}
	.hist__under {
		display: none;
		overflow: hidden;
		font-size: 0.75rem;
		text-overflow: ellipsis;
		color: var(--dn-muted);
	}
	@container (max-width: 34rem) {
		.hist__wide {
			display: none;
		}
		.hist__under {
			display: block;
		}
	}
</style>
