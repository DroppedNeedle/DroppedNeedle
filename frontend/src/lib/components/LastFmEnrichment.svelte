<script lang="ts">
	import type { LastFmArtistEnrichment } from '$lib/types';
	import LastFmInfoCard from './LastFmInfoCard.svelte';

	interface Props {
		enrichment: LastFmArtistEnrichment | null | undefined;
		loading?: boolean;
		enabled?: boolean;
	}

	let { enrichment, loading = false, enabled = true }: Props = $props();
</script>

<LastFmInfoCard
	text={enrichment?.bio}
	tags={enrichment?.tags ?? []}
	listeners={enrichment?.listeners}
	playcount={enrichment?.playcount}
	url={enrichment?.url}
	{loading}
	{enabled}
/>
