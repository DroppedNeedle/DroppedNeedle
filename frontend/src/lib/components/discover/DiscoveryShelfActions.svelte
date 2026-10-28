<script lang="ts">
	import { Download } from 'lucide-svelte';
	import DiscoveryBatchModal from '$lib/components/discover/DiscoveryBatchModal.svelte';
	import RadioPlayButton from '$lib/components/discover/RadioPlayButton.svelte';
	import { libraryStore } from '$lib/stores/library';
	import Button from '$lib/ui/Button.svelte';
	import type { ChartSection, RadioPlanRequest } from '$lib/queries/discover/types';
	import { albumRows } from '$lib/utils/sectionRows';

	interface Props {
		section: ChartSection;
		sectionKey: string;
		seed: Omit<RadioPlanRequest, 'exclude_recording_mbids' | 'fast'>;
	}

	let { section, sectionKey, seed }: Props = $props();
	let batchModalOpen = $state(false);

	const albums = $derived(albumRows(section));
	const requestableCount = $derived(
		albums.filter(
			(album) =>
				album.musicbrainz_id &&
				!album.in_library &&
				!album.requested &&
				!libraryStore.isRequested(album.musicbrainz_id)
		).length
	);
	const canDownload = $derived(requestableCount > 0);
</script>

<div class="flex flex-wrap items-center gap-2">
	<RadioPlayButton {seed} size="sm" label="Play all" />
	{#if canDownload}
		<Button
			variant="outline"
			size="sm"
			icon={Download}
			title="Request every album you don't own"
			onclick={() => (batchModalOpen = true)}
		>
			Download all
		</Button>
	{/if}
</div>

{#if canDownload}
	<DiscoveryBatchModal
		bind:open={batchModalOpen}
		sectionTitle={section.title}
		{sectionKey}
		{albums}
	/>
{/if}
