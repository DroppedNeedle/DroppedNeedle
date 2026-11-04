<!-- Where a shelf's picks come from: an acid label for an outside source, a plain one for the library. -->
<script lang="ts">
	import { Radio, Headphones } from 'lucide-svelte';
	import type { components } from '$lib/api/v3/openapi';
	import StatusChip from '$lib/ui/StatusChip.svelte';

	interface Props {
		source?: components['schemas']['ShelfSource'];
	}

	let { source }: Props = $props();
</script>

{#snippet lastfm()}
	<StatusChip tone="action" size="xs" icon={Radio}>Last.fm</StatusChip>
{/snippet}

{#snippet listenbrainz()}
	<StatusChip tone="action" size="xs" icon={Headphones}>ListenBrainz</StatusChip>
{/snippet}

{#if source === 'lastfm'}
	{@render lastfm()}
{:else if source === 'listenbrainz'}
	{@render listenbrainz()}
{:else if source === 'blended'}
	<span class="inline-flex gap-1">
		{@render lastfm()}
		{@render listenbrainz()}
	</span>
{:else if source === 'library' || source === 'local'}
	<StatusChip size="xs">{source}</StatusChip>
{:else if source}
	<StatusChip tone="action" size="xs">{source}</StatusChip>
{/if}
