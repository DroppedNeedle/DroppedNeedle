<script lang="ts">
	import type { components } from '$lib/api/v3/openapi';
	import LastFmInfoCard from './LastFmInfoCard.svelte';

	type LastFmArtistEnrichment = components['schemas']['LastFmArtistEnrichment'];

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
