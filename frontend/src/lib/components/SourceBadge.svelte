<script lang="ts">
	import { Radio, Headphones } from 'lucide-svelte';

	interface Props {
		source?: string;
	}

	let { source }: Props = $props();
</script>

{#snippet lastfm()}
	<span class="badge badge-xs sm:badge-sm border-0 gap-1 bg-lastfm/15 text-lastfm">
		<Radio class="w-2.5 h-2.5 sm:w-3 sm:h-3" />
		Last.fm
	</span>
{/snippet}

{#snippet listenbrainz()}
	<span class="badge badge-xs sm:badge-sm border-0 gap-1 bg-listenbrainz/15 text-listenbrainz">
		<Headphones class="w-2.5 h-2.5 sm:w-3 sm:h-3" />
		ListenBrainz
	</span>
{/snippet}

{#if source === 'lastfm'}
	{@render lastfm()}
{:else if source === 'listenbrainz'}
	{@render listenbrainz()}
{:else if source === 'lastfm_and_listenbrainz'}
	<span class="inline-flex gap-1">
		{@render lastfm()}
		{@render listenbrainz()}
	</span>
{:else if source}
	<span class="badge badge-ghost badge-xs sm:badge-sm capitalize">{source}</span>
{/if}
