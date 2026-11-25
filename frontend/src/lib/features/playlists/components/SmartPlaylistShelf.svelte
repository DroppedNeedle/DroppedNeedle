<script lang="ts">
	import { ListMusic, Plus } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { getSmartPlaylistsQuery } from '../queries/SmartPlaylistQueries.svelte';
	import SmartPlaylistEditor from './SmartPlaylistEditor.svelte';

	const lists = getSmartPlaylistsQuery();
	let creating = $state(false);
</script>

<section>
	<SectionHead title="Smart playlists">
		{#snippet actions()}
			<Button variant="outline" size="sm" icon={Plus} onclick={() => (creating = true)}
				>New smart playlist</Button
			>
		{/snippet}
	</SectionHead>
	<QueryBoundary query={lists} what="smart playlists">
		{#snippet children(items)}
			<ul class="divide-y divide-base-300">
				{#each items as list (list.id)}
					<li>
						<a
							class="flex items-center gap-3 py-3 font-semibold hover:underline"
							href={withBasePath('/playlists/smart/' + encodeURIComponent(list.id))}
						>
							<ListMusic class="size-5 shrink-0" aria-hidden="true" />
							<span class="min-w-0 truncate">{list.name}</span>
						</a>
					</li>
				{/each}
			</ul>
		{/snippet}
		{#snippet empty()}<p class="text-sm text-muted">
				Save a rating rule to collect matching tracks from your library.
			</p>{/snippet}
	</QueryBoundary>
</section>
{#if creating}
	<SmartPlaylistEditor onclose={() => (creating = false)} />
{/if}
