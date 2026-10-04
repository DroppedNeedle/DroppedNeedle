<script lang="ts">
	import type { HomeSection as HomeSectionType, HomeAlbum, RadioSeedItem } from '$lib/types';
	import HomeSection from '$lib/components/HomeSection.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import DiscoveryShelfActions from '$lib/components/discover/DiscoveryShelfActions.svelte';
	import { ChevronDown, Disc3 } from 'lucide-svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import { tilt } from '$lib/actions/tilt';
	import { slide } from 'svelte/transition';

	interface Props {
		section: HomeSectionType;
	}

	let { section }: Props = $props();

	let expanded = $state(false);

	const albumItems = $derived(
		section.items.filter((_item): _item is HomeAlbum => section.type === 'albums')
	);
	const mosaicAlbums = $derived(albumItems.slice(0, 4));
	const albumCount = $derived(albumItems.length);
	const seedItems = $derived.by(() => {
		const items: RadioSeedItem[] = [];
		const seen = new SvelteSet<string>();
		for (const album of albumItems) {
			if (!album.artist_mbid || !album.mbid || seen.has(album.mbid)) continue;
			seen.add(album.mbid);
			items.push({
				artist_mbid: album.artist_mbid,
				artist_name: album.artist_name ?? '',
				album_mbid: album.mbid,
				album_name: album.name
			});
		}
		return items;
	});

	function toggleExpanded() {
		expanded = !expanded;
	}
</script>

<div class="w-full">
	<div style="perspective: 1200px;">
		<button
			use:tilt={{ shadowColorVar: 'var(--color-primary)' }}
			type="button"
			class="group relative w-full overflow-hidden rounded-2xl border transition-all cursor-pointer text-left
				focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100
				active:scale-[0.98]
				{expanded ? 'border-primary/30' : 'border-white/10'}"
			style="
				transform-style: preserve-3d;
				transform: var(--tilt-transform, rotateX(0) rotateY(0));
				box-shadow: var(--tilt-shadow);
				transition: transform 0.5s var(--ease-spring), box-shadow 0.5s var(--ease-spring), border-color 0.3s ease;
			"
			onclick={toggleExpanded}
			aria-expanded={expanded}
			aria-label="{section.title} - {section.items.length} albums. {expanded
				? 'Collapse'
				: 'Expand'} to browse."
		>
			<div class="absolute inset-0 grid grid-cols-2 grid-rows-2" style="height: 160px;">
				{#each mosaicAlbums as album, i (album.mbid ?? `${album.name}-${i}`)}
					<div class="relative overflow-hidden">
						<AlbumImage
							mbid={album.mbid || ''}
							alt={album.name}
							size="sm"
							rounded="none"
							className="w-full h-full"
							customUrl={album.image_url || null}
						/>
					</div>
				{:else}
					{#each Array(4) as _, i (i)}
						<div class="bg-base-200 flex items-center justify-center">
							<Disc3 class="h-6 w-6 text-base-content/20" />
						</div>
					{/each}
				{/each}
			</div>

			<div
				class="absolute inset-0 bg-gradient-to-t from-black/70 via-black/40 to-black/20"
				style="height: 160px;"
			></div>

			<div
				class="pointer-events-none absolute inset-0 rounded-2xl bg-gradient-to-br from-white/[0.04] to-transparent"
				style="height: 160px;"
			></div>

			<div
				class="pointer-events-none absolute inset-0 rounded-2xl"
				style="background: var(--tilt-specular-bg, transparent); height: 160px;"
			></div>

			<div class="relative flex items-end justify-between p-4" style="height: 160px;">
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
		<div class="mt-3 rounded-2xl border border-primary/15 bg-base-200/45 p-4">
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
		</div>
	</div>
{/if}
