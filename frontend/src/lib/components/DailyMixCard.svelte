<script lang="ts">
	import type { RadioSeedItem } from '$lib/types';
	import type { ChartSection as HomeSectionType } from '$lib/queries/discover/types';
	import { albumRows } from '$lib/utils/sectionRows';
	import HomeSection from '$lib/components/HomeSection.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import DiscoveryShelfActions from '$lib/components/discover/DiscoveryShelfActions.svelte';
	import { ChevronDown, Disc3 } from 'lucide-svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import { tilt } from '$lib/actions/tilt';
	import { slide } from '$lib/ui/motion';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		section: HomeSectionType;
	}

	let { section }: Props = $props();

	let expanded = $state(false);

	const albumItems = $derived(albumRows(section));
	const mosaicAlbums = $derived(albumItems.slice(0, 4));
	const albumCount = $derived(albumItems.length);
	const seedItems = $derived.by(() => {
		const items: RadioSeedItem[] = [];
		const seen = new SvelteSet<string>();
		for (const album of albumItems) {
			if (!album.artist_id || !album.musicbrainz_id || seen.has(album.musicbrainz_id)) continue;
			seen.add(album.musicbrainz_id);
			items.push({
				artist_mbid: album.artist_id,
				artist_name: album.artist_name ?? '',
				album_mbid: album.musicbrainz_id,
				album_name: album.title
			});
		}
		return items;
	});

	function toggleExpanded() {
		expanded = !expanded;
	}

	const TILT_TRANSITION =
		'transform 0.5s var(--ease-spring), box-shadow 0.5s var(--ease-spring), border-color 0.3s ease';
</script>

<div class="w-full">
	<div class="perspective-[1200px]">
		<button
			use:tilt={{ shadowColorVar: 'var(--color-primary)' }}
			type="button"
			class="group relative w-full overflow-hidden rounded-2xl border cursor-pointer text-left
				focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100
				active:scale-[0.98]
				{expanded ? 'border-primary/30' : 'border-base-content/10'}"
			style:transform-style="preserve-3d"
			style:transform="var(--tilt-transform, rotateX(0) rotateY(0))"
			style:box-shadow="var(--tilt-shadow)"
			style:transition={TILT_TRANSITION}
			onclick={toggleExpanded}
			aria-expanded={expanded}
			aria-label="{section.title} - {section.items.length} albums. {expanded
				? 'Collapse'
				: 'Expand'} to browse."
		>
			<div class="absolute inset-0 grid h-[160px] grid-cols-2 grid-rows-2">
				{#each mosaicAlbums as album, i (album.musicbrainz_id ?? `${album.title}-${i}`)}
					<div class="relative overflow-hidden">
						<AlbumImage
							mbid={album.musicbrainz_id || ''}
							alt={album.title}
							size="sm"
							rounded="none"
							className="w-full h-full"
							customUrl={album.cover_url || null}
						/>
					</div>
				{:else}
					{#each Array(4) as _, i (i)}
						<div class="relative overflow-hidden">
							<CoverPlaceholder name={`${section.title}-${i}`} />
						</div>
					{/each}
				{/each}
			</div>

			<div
				class="absolute inset-0 h-[160px] bg-gradient-to-t from-black/70 via-black/40 to-black/20"
			></div>

			<div
				class="pointer-events-none absolute inset-0 h-[160px] rounded-2xl bg-gradient-to-br from-white/[0.04] to-transparent"
			></div>

			<div
				class="pointer-events-none absolute inset-0 h-[160px] rounded-2xl"
				style:background="var(--tilt-specular-bg, transparent)"
			></div>

			<div class="relative flex h-[160px] items-end justify-between p-4">
				<div class="flex flex-col gap-1.5 min-w-0 flex-1 mr-3">
					<h3
						class="text-sm sm:text-base font-bold text-white leading-tight line-clamp-2 drop-shadow-lg"
					>
						{section.title}
					</h3>
					<div class="flex items-center gap-2">
						<span
							class="inline-flex items-center gap-1 rounded-full bg-black/40 backdrop-blur-sm px-2 py-0.5 text-xs text-white/70"
						>
							<Disc3 class="h-3 w-3" />
							{albumCount} album{albumCount !== 1 ? 's' : ''}
						</span>
					</div>
				</div>

				<div
					class="flex items-center justify-center h-8 w-8 rounded-full bg-black/40 backdrop-blur-sm border border-white/10 transition-all
						group-hover:bg-primary/20 group-hover:border-primary/30"
				>
					<ChevronDown
						class="h-4 w-4 text-white/80 transition-transform duration-300
							{expanded ? 'rotate-180' : ''}"
					/>
				</div>
			</div>
		</button>
	</div>
</div>

{#if expanded}
	<div transition:slide={{ duration: 300 }} class="col-span-full overflow-hidden">
		<Card variant="bordered" padding="md" class="mt-3">
			<div class="mb-4 flex flex-wrap items-center justify-between gap-3">
				<p class="text-sm text-base-content/60">
					Play the mix, or download any albums you're missing.
				</p>
				<DiscoveryShelfActions
					{section}
					sectionKey="daily_mixes"
					seed={{ seed_type: 'items', items: seedItems }}
				/>
			</div>
			<HomeSection {section} showConnectCard={false} />
		</Card>
	</div>
{/if}
