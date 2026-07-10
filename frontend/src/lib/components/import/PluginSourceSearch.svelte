<script lang="ts">
	import { Download, Search } from 'lucide-svelte';
	import {
		getSourcePluginsQuery,
		getSourceSearchQuery
	} from '$lib/queries/plugins/PluginQueries.svelte';
	import { fetchFromSourceMutation } from '$lib/queries/plugins/PluginMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';

	const sourcesQuery = getSourcePluginsQuery(() => authStore.isTrusted);
	const sources = $derived(sourcesQuery.data?.sources ?? []);

	let selected = $state('');
	let query = $state('');
	const activeSource = $derived(selected || sources[0]?.name || '');

	const searchQuery = getSourceSearchQuery(
		() => activeSource,
		() => query
	);
	const fetchMutation = fetchFromSourceMutation();
	const items = $derived(searchQuery.data?.items ?? []);
</script>

{#if sources.length > 0}
	<!-- Only rendered when an enabled source plugin exists - the section is
	     plugin-provided by construction, DN ships no sources. -->
	<div class="mb-6 rounded-2xl border border-base-content/10 bg-base-200/40 p-4">
		<div class="flex flex-wrap items-center gap-2">
			<h3 class="text-sm font-semibold">Find music in your sources</h3>
			{#if sources.length > 1}
				<select
					class="select select-bordered select-xs"
					bind:value={selected}
					aria-label="Source plugin"
				>
					{#each sources as source (source.name)}
						<option value={source.name}>{source.display_name}</option>
					{/each}
				</select>
			{:else}
				<span class="badge badge-ghost badge-sm">{sources[0].display_name}</span>
			{/if}
		</div>

		<label class="input input-bordered input-sm mt-3 flex w-full items-center gap-2">
			<Search class="h-4 w-4 opacity-50" aria-hidden="true" />
			<input
				type="text"
				class="grow"
				placeholder="Search…"
				bind:value={query}
				aria-label="Search this source"
			/>
			{#if searchQuery.isFetching}<span class="loading loading-spinner loading-xs"></span>{/if}
		</label>

		{#if items.length}
			<ul class="mt-3 max-h-72 space-y-1 overflow-y-auto">
				{#each items as item (item.id)}
					<li
						class="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-base-100/60 px-3 py-2"
					>
						<div class="min-w-0">
							<p class="truncate text-sm font-medium">{item.title}</p>
							<p class="truncate text-xs text-base-content/50">
								{item.artist}{item.detail ? ` · ${item.detail}` : ''}
							</p>
						</div>
						<button
							class="btn btn-primary btn-xs shrink-0"
							onclick={() => fetchMutation.mutate({ plugin: activeSource, itemId: item.id })}
							disabled={fetchMutation.isPending}
						>
							<Download class="h-3.5 w-3.5" aria-hidden="true" />
							Fetch
						</button>
					</li>
				{/each}
			</ul>
		{:else if query.trim().length >= 2 && !searchQuery.isFetching}
			<p class="mt-3 text-sm text-base-content/50">Nothing found - try another search.</p>
		{/if}
	</div>
{/if}
