<script lang="ts">
	import { Download } from 'lucide-svelte';
	import DiscoveryBatchModal from '$lib/features/discover/components/DiscoveryBatchModal.svelte';
	import RadioPlayButton from '$lib/features/discover/components/RadioPlayButton.svelte';
	import { libraryStore } from '$lib/stores/library';
	import Button from '$lib/ui/Button.svelte';
	import type { ChartSection, RadioPlanRequest } from '$lib/features/discover/queries/types';
	import type { PlayedFrom } from '$lib/features/player/playback/types';
	import { albumRows } from '$lib/features/discover/components/sectionRows';

	interface Props {
		section: ChartSection;
		sectionKey: string;
		seed: Omit<RadioPlanRequest, 'exclude_recording_mbids' | 'fast'>;
		/** What the shelf's plays say they came from, when not a station. */
		playedFrom?: PlayedFrom;
	}

	let { section, sectionKey, seed, playedFrom }: Props = $props();
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
	<RadioPlayButton {seed} {playedFrom} size="sm" label="Play all" />
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
