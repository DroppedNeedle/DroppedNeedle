<!-- A result's listen count in mono, with the service it came from. -->
<script lang="ts">
	import { Headphones } from 'lucide-svelte';
	import type { EnrichmentSource } from '$lib/features/album/queries/types';
	import { getListenTitle } from '$lib/features/search/queries/enrichment';
	import { formatListenCount } from '$lib/utils/formatting';

	interface Props {
		count: number;
		source: EnrichmentSource;
		kind: 'artist' | 'album';
	}

	let { count, source, kind }: Props = $props();

	const service = $derived(
		source === 'lastfm' ? 'Last.fm' : source === 'listenbrainz' ? 'LB' : null
	);
</script>

<span
	class="inline-flex items-center gap-1 font-mono text-xs tracking-wide text-muted uppercase"
	title={getListenTitle(source, kind)}
>
	<Headphones class="size-3 shrink-0" aria-hidden="true" />
	<span class="font-semibold text-base-content">{formatListenCount(count, true)}</span>
	{#if service}<span>{service}</span>{:else}<span>listens</span>{/if}
</span>
