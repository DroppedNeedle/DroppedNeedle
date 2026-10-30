<script lang="ts">
	import type { LastFmAlbumEnrichment } from '$lib/types';
	import LastFmInfoCard from './LastFmInfoCard.svelte';

	interface Props {
		enrichment: LastFmAlbumEnrichment | null;
		loading?: boolean;
		enabled?: boolean;
	}

	let { enrichment, loading = false, enabled = true }: Props = $props();
</script>

<LastFmInfoCard
	text={enrichment?.summary}
	tags={enrichment?.tags ?? []}
	listeners={enrichment?.listeners}
	playcount={enrichment?.playcount}
	url={enrichment?.url}
	{loading}
	{enabled}
/>
