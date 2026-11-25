<script lang="ts">
	import {
		buildDiscoveryQueueFromLocal,
		withPlayedFrom
	} from '$lib/features/player/playback/queueHelpers';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { Play } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { getSmartTracksQuery } from '../queries/SmartPlaylistQueries.svelte';

	let { id }: { id: string } = $props();
	let cursors = $state<string[]>([]);
	const tracks = getSmartTracksQuery(
		() => id,
		() => cursors.at(-1) ?? null
	);
	function nextPage() {
		const next = tracks.data?.next;
		if (next) cursors.push(next);
	}
</script>

<QueryBoundary query={tracks} what="matching tracks">
	{#snippet children(page)}
		<p class="text-sm text-muted">
			{page.total.toLocaleString()} matching {page.total === 1 ? 'track' : 'tracks'}
		</p>
		<ul class="divide-y divide-base-300">
			{#each page.items as track (track.id)}
				<li class="flex items-center gap-3 py-2">
					<IconButton
						icon={Play}
						label={'Play ' + track.title}
						onclick={() =>
							playerStore.playQueue(
								withPlayedFrom(buildDiscoveryQueueFromLocal([track]), { kind: 'library' }),
								0,
								false
							)}
					/>
					<div class="min-w-0">
						<p class="truncate font-semibold">{track.title}</p>
						<p class="truncate text-sm text-muted">{track.artist_name}</p>
					</div>
				</li>
			{/each}
		</ul>
	{/snippet}
	{#snippet empty()}
		<p class="text-muted">No tracks on this page meet the rating rule.</p>
	{/snippet}
</QueryBoundary>
{#if cursors.length || tracks.data?.next}
	<nav aria-label="Matching track pages" class="flex gap-2">
		<Button variant="outline" disabled={!cursors.length} onclick={() => cursors.pop()}
			>Previous</Button
		>
		<Button variant="outline" disabled={!tracks.data?.next} onclick={nextPage}>Next</Button>
	</nav>
{/if}
