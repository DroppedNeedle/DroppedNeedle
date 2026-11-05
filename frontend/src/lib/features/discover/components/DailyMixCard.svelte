<script lang="ts">
	import type {
		RadioSeedItem,
		ChartSection as HomeSectionType
	} from '$lib/features/discover/queries/types';
	import { albumRows } from '$lib/features/discover/components/sectionRows';
	import HomeSection from '$lib/features/discover/components/HomeSection.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import DiscoveryShelfActions from '$lib/features/discover/components/DiscoveryShelfActions.svelte';
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
			class="group relative w-full overflow-hidden rounded-xl border bg-base-200 cursor-pointer text-left
				focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100
				active:scale-[0.98]
				{expanded ? 'border-action' : 'border-line'}"
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
			<div
				class="pointer-events-none absolute inset-0 rounded-xl"
				style:background="var(--tilt-specular-bg, transparent)"
			></div>

			<div class="relative flex h-32 items-stretch">
				<div class="grid aspect-square h-full shrink-0 grid-cols-2 grid-rows-2">
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

				<div class="flex min-w-0 flex-1 items-center gap-3 px-4">
					<div class="flex min-w-0 flex-1 flex-col gap-2">
						<h3 class="font-display text-xl leading-none uppercase line-clamp-2 sm:text-2xl">
							{section.title}
						</h3>
						<span
							class="inline-flex w-fit items-center gap-1 rounded-full border border-edge px-2 py-0.5 font-mono text-[0.6875rem] font-semibold tracking-wide"
						>
							<Disc3 class="h-3 w-3" aria-hidden="true" />
							{albumCount} album{albumCount !== 1 ? 's' : ''}
						</span>
					</div>

					<span
						class="flex h-8 w-8 shrink-0 items-center justify-center rounded-full border border-edge transition-colors
							group-hover:border-action group-hover:bg-action group-hover:text-action-content
							{expanded ? 'border-action bg-action text-action-content' : ''}"
					>
						<ChevronDown
							class="h-4 w-4 transition-transform duration-300 {expanded ? 'rotate-180' : ''}"
						/>
					</span>
				</div>
			</div>
		</button>
	</div>
</div>

{#if expanded}
	<div transition:slide={{ duration: 300 }} class="col-span-full overflow-hidden">
		<Card variant="bordered" padding="md" class="mt-1">
			<div class="mb-4 flex flex-wrap items-center justify-between gap-3">
				<p class="text-sm text-muted">Play the mix, or download any albums you're missing.</p>
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
