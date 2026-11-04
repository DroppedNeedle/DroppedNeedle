<script lang="ts">
	import type { components } from '$lib/api/v3/openapi';
	import LastFmInfoCard from './LastFmInfoCard.svelte';

	type LastFmAlbumEnrichment = components['schemas']['LastFmAlbumEnrichment'];

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
