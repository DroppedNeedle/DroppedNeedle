<script lang="ts">
	import type { HomeSection as HomeSectionType, HomeAlbum } from '$lib/types';
	import HomeSection from '$lib/components/HomeSection.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import { getDiscoverRadioV3Query } from '$lib/queries/discover/DiscoverV3Queries.svelte';
	import { toHomeSection } from '$lib/queries/discover/DiscoverV3Adapters';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { ChevronDown, Disc3, Radio, RefreshCw } from 'lucide-svelte';
	import DiscoveryShelfActions from '$lib/components/discover/DiscoveryShelfActions.svelte';
	import { tilt } from '$lib/actions/tilt';
	import { slide } from 'svelte/transition';

	interface Props {
		seedType: string;
		seedId: string;
		initialSection?: HomeSectionType | null;
	}

	let { seedType, seedId, initialSection = null }: Props = $props();

	let expanded = $state(false);
	const radioQuery = getDiscoverRadioV3Query(() => ({ seedType, seedId, enabled: expanded }));
	const freshSection = $derived(radioQuery.data ? toHomeSection(radioQuery.data) : undefined);
	const section = $derived(
		freshSection?.items.length ? freshSection : (initialSection ?? freshSection)
	);

	const albumItems = $derived(
		section ? section.items.filter((_item): _item is HomeAlbum => section.type === 'albums') : []
	);
	const featuredAlbum = $derived(albumItems[0] ?? null);
	const albumCount = $derived(albumItems.length);

	function toggleExpanded() {
		expanded = !expanded;
	}

	async function handleRefresh() {
		await invalidateQueriesWithPersister({
			queryKey: DiscoverQueryKeyFactory.v3.radio(authStore.user?.id, seedType, seedId, {})
		});
	}
</script>

{#if section}
	<div class="w-full">
		<div style="perspective: 1200px;">
			<button
				use:tilt={{ shadowColorVar: 'var(--color-secondary)' }}
				type="button"
				class="group relative w-full overflow-hidden rounded-2xl border transition-all cursor-pointer text-left
					focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-secondary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100
					active:scale-[0.98]
					{expanded ? 'border-secondary/30' : 'border-secondary/15'}"
				style="
					transform-style: preserve-3d;
					transform: var(--tilt-transform, rotateX(0) rotateY(0));
					box-shadow: var(--tilt-shadow);
					transition: transform 0.5s var(--ease-spring), box-shadow 0.5s var(--ease-spring), border-color 0.3s ease;
				"
				onclick={toggleExpanded}
				aria-expanded={expanded}
				aria-label="{section.title} radio. {albumCount
					? `${albumCount} album${albumCount === 1 ? '' : 's'}.`
					: 'Tracks load when you press Play all.'} {expanded ? 'Collapse' : 'Expand'} to browse."
			>
				<div class="absolute inset-0" style="height: 140px;">
					{#if featuredAlbum}
						<AlbumImage
							mbid={featuredAlbum.mbid || ''}
							alt={featuredAlbum.name}
							size="md"
							rounded="none"
							className="w-full h-full object-cover"
							customUrl={featuredAlbum.image_url || null}
						/>
					{:else if seedType === 'artist' && seedId}
						<ArtistImage
							mbid={seedId}
							alt={section.title}
							size="full"
							requestSize={250}
							rounded="none"
							className="h-full w-full object-cover"
						/>
					{:else}
						<div class="flex h-full w-full items-center justify-center bg-base-200">
							<Disc3 class="h-8 w-8 text-base-content/20" />
						</div>
					{/if}
				</div>

				<div
					class="absolute inset-0 bg-gradient-to-t from-black/75 via-black/45 to-black/25"
					style="height: 140px;"
				></div>

				<div
					class="radio-waves pointer-events-none absolute inset-0 overflow-hidden"
					style="height: 140px;"
					aria-hidden="true"
				>
					<div class="wave wave-1"></div>
					<div class="wave wave-2"></div>
					<div class="wave wave-3"></div>
				</div>

				<div
					class="pointer-events-none absolute inset-0 rounded-2xl bg-gradient-to-br from-secondary/[0.06] to-transparent"
					style="height: 140px;"
				></div>

				<div
					class="pointer-events-none absolute inset-0 rounded-2xl"
					style="background: var(--tilt-specular-bg, transparent); height: 140px;"
				></div>

				<div class="absolute right-3 top-3 opacity-20" style="height: auto;">
					<Radio class="h-6 w-6 text-white" />
				</div>

				<div class="relative flex items-end justify-between p-4" style="height: 140px;">
					<div class="mr-3 flex min-w-0 flex-1 flex-col gap-1.5">
						<h3
							class="line-clamp-2 text-sm font-bold leading-tight text-white drop-shadow-lg sm:text-base"
						>
							{section.title}
						</h3>
						<div class="flex items-center gap-2">
							<span
								class="inline-flex items-center gap-1 rounded-full bg-black/40 px-2 py-0.5 text-xs text-white/70 backdrop-blur-sm"
							>
								<Radio class="h-3 w-3" />
								{albumCount ? `${albumCount} album${albumCount === 1 ? '' : 's'}` : 'Ready to play'}
							</span>
						</div>
					</div>

					<div
						class="flex h-8 w-8 items-center justify-center rounded-full border border-white/10 bg-black/40 backdrop-blur-sm transition-all
							group-hover:border-secondary/30 group-hover:bg-secondary/20"
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
			<div class="mt-3 rounded-2xl border border-secondary/15 bg-base-200/45 p-4">
				<div class="mb-4 flex flex-wrap items-center justify-between gap-3">
					<p class="text-sm text-base-content/60">
						{#if albumCount}
							Play the station, or download any albums you're missing.
						{:else if radioQuery.isFetching}
							Finding albums for this station...
						{:else}
							The complete track list is built when you press play.
						{/if}
					</p>
					<div class="flex items-center gap-2">
						<DiscoveryShelfActions
							{section}
							sectionKey="radio_sections"
							seed={{
								seed_type:
									seedType === 'genre' ? 'genre' : seedType === 'album' ? 'album' : 'artist',
								seed_id: seedId
							}}
						/>
						<button
							class="btn btn-ghost btn-sm gap-1"
							onclick={handleRefresh}
							disabled={radioQuery.isFetching}
						>
							<RefreshCw class="h-3.5 w-3.5 {radioQuery.isFetching ? 'animate-spin' : ''}" />
							Refresh
						</button>
					</div>
				</div>
				{#if albumCount}<HomeSection {section} showConnectCard={false} />{/if}
			</div>
		</div>
	{/if}
{/if}

<style>
	.radio-waves {
		--wave-color: oklch(from var(--color-secondary) l c h / 0.12);
	}

	.wave {
		position: absolute;
		top: -20px;
		right: -20px;
		border-radius: 50%;
		border: 1.5px solid var(--wave-color);
		opacity: 0;
		animation: radio-pulse 3s ease-out infinite;
	}

	.wave-1 {
		width: 60px;
		height: 60px;
		animation-delay: 0s;
	}

	.wave-2 {
		width: 100px;
		height: 100px;
		animation-delay: 1s;
	}

	.wave-3 {
		width: 140px;
		height: 140px;
		animation-delay: 2s;
	}

	@keyframes radio-pulse {
		0% {
			opacity: 0;
			transform: scale(0.5);
		}
		30% {
			opacity: 1;
		}
		100% {
			opacity: 0;
			transform: scale(1);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.wave {
			animation: none;
			opacity: 0.5;
			transform: scale(1);
		}
	}
</style>
