<script lang="ts">
	import { Download } from 'lucide-svelte';
	import HomeSection from '$lib/components/HomeSection.svelte';
	import RadioPlayButton from '$lib/components/discover/RadioPlayButton.svelte';
	import DiscoveryBatchModal from '$lib/components/discover/DiscoveryBatchModal.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { SvelteSet } from 'svelte/reactivity';
	import Button from '$lib/ui/Button.svelte';
	import type { HomeAlbum, HomeSection as HomeSectionType, RadioSeedItem } from '$lib/types';

	interface Props {
		section: HomeSectionType;
		headerLink?: string | null;
		/** stable section key for batch attribution ("daily_mixes", "top_picks", ...) */
		sectionKey?: string;
		downloadable?: boolean;
	}

	let { section, headerLink = null, sectionKey = '', downloadable = true }: Props = $props();

	let batchModalOpen = $state(false);

	const albums = $derived(section.items as HomeAlbum[]);
	const requestableCount = $derived(albums.filter((a) => a.mbid && !a.in_library).length);
	const showDownloadAll = $derived(
		downloadable && $integrationStore.download_client && requestableCount > 1
	);

	const seedItems = $derived.by(() => {
		const items: RadioSeedItem[] = [];
		const seen = new SvelteSet<string>();
		for (const item of section.items as HomeAlbum[]) {
			const mbid = item.artist_mbid;
			if (!mbid || !item.mbid || seen.has(item.mbid)) continue;
			seen.add(item.mbid);
			items.push({
				artist_mbid: mbid,
				artist_name: item.artist_name ?? '',
				album_mbid: item.mbid,
				album_name: item.name
			});
		}
		return items;
	});
</script>

<HomeSection {section} {headerLink}>
	{#snippet headerActions()}
		<div class="flex items-center gap-1">
			{#if seedItems.length > 0}
				<RadioPlayButton
					seed={{ seed_type: 'items', items: seedItems }}
					size="xs"
					variant="ghost"
					label="Play"
				/>
			{/if}
			{#if showDownloadAll}
				<Button
					variant="ghost"
					size="xs"
					icon={Download}
					title="Request every album in this section as a removable batch"
					onclick={() => (batchModalOpen = true)}
				>
					All
				</Button>
			{/if}
		</div>
	{/snippet}
</HomeSection>

{#if showDownloadAll}
	<DiscoveryBatchModal
		bind:open={batchModalOpen}
		sectionTitle={section.title}
		{sectionKey}
		{albums}
	/>
{/if}
