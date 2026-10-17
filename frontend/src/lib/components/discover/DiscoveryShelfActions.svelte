<script lang="ts">
	import { Download } from 'lucide-svelte';
	import DiscoveryBatchModal from '$lib/components/discover/DiscoveryBatchModal.svelte';
	import RadioPlayButton from '$lib/components/discover/RadioPlayButton.svelte';
	import { libraryStore } from '$lib/stores/library';
	import Button from '$lib/ui/Button.svelte';
	import type { HomeAlbum, HomeSection, RadioPlanRequest } from '$lib/types';

	interface Props {
		section: HomeSection;
		sectionKey: string;
		seed: Omit<RadioPlanRequest, 'exclude_recording_mbids' | 'fast'>;
	}

	let { section, sectionKey, seed }: Props = $props();
	let batchModalOpen = $state(false);

	const albums = $derived(
		section.type === 'albums' ? (section.items as HomeAlbum[]) : ([] as HomeAlbum[])
	);
	const requestableCount = $derived(
		albums.filter(
			(album) =>
				album.mbid && !album.in_library && !album.requested && !libraryStore.isRequested(album.mbid)
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
