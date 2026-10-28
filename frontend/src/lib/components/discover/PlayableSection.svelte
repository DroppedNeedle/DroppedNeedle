<script lang="ts">
	import { Download } from 'lucide-svelte';
	import HomeSection from '$lib/components/HomeSection.svelte';
	import RadioPlayButton from '$lib/components/discover/RadioPlayButton.svelte';
	import DiscoveryBatchModal from '$lib/components/discover/DiscoveryBatchModal.svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import Button from '$lib/ui/Button.svelte';
	import type { RadioSeedItem } from '$lib/types';
	import type { ChartSection as HomeSectionType } from '$lib/queries/discover/types';
	import { albumRows } from '$lib/utils/sectionRows';

	interface Props {
		section: HomeSectionType;
		headerLink?: string | null;
		/** stable section key for batch attribution ("daily_mixes", "top_picks", ...) */
		sectionKey?: string;
		downloadable?: boolean;
	}

	let { section, headerLink = null, sectionKey = '', downloadable = true }: Props = $props();

	let batchModalOpen = $state(false);

	const albums = $derived(albumRows(section));
	const requestableCount = $derived(albums.filter((a) => a.musicbrainz_id && !a.in_library).length);
	const showDownloadAll = $derived(downloadable && requestableCount > 1);

	const seedItems = $derived.by(() => {
		const items: RadioSeedItem[] = [];
		const seen = new SvelteSet<string>();
		for (const item of albums) {
			const mbid = item.artist_id;
			if (!mbid || !item.musicbrainz_id || seen.has(item.musicbrainz_id)) continue;
			seen.add(item.musicbrainz_id);
			items.push({
				artist_mbid: mbid,
				artist_name: item.artist_name ?? '',
				album_mbid: item.musicbrainz_id,
				album_name: item.title
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
