<script lang="ts">
	import { coverSrc } from '$lib/api/covers';
	import { ChevronLeft, ChevronRight, Sparkles, ThumbsDown } from 'lucide-svelte';
	import { fly } from '$lib/ui/motion';
	import type { TopPickItem, TopPicksSection } from '$lib/queries/discover/types';
	import { SvelteSet } from 'svelte/reactivity';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import AlbumRequestButton from '$lib/components/AlbumRequestButton.svelte';
	import RadioPlayButton from '$lib/components/discover/RadioPlayButton.svelte';
	import SampleButton from '$lib/components/discover/SampleButton.svelte';
	import SourceBadge from '$lib/components/SourceBadge.svelte';
	import LiveUpdatingBadge from '$lib/components/LiveUpdatingBadge.svelte';
	import HeroBackdrop from '$lib/components/HeroBackdrop.svelte';
	import HorizontalCarousel from '$lib/components/HorizontalCarousel.svelte';
	import { albumHrefOrNull, artistHrefOrNull } from '$lib/utils/entityRoutes';
	import { libraryStore } from '$lib/stores/library';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		section: TopPicksSection;
		updating?: boolean;
		onignore?: (pick: TopPickItem) => Promise<void>;
	}

	let { section, updating = false, onignore }: Props = $props();

	let featuredIndex = $state(0);
	let slideDirection = $state(1);

	const dismissed = new SvelteSet<string>();
	let dismissing = $state(false);
	const picks = $derived(
		section.items.filter(
			(pick) => !pick.album.musicbrainz_id || !dismissed.has(pick.album.musicbrainz_id)
		)
	);
	const featured = $derived(picks[featuredIndex]);
	const featuredHref = $derived(featured ? albumHrefOrNull(featured.album.musicbrainz_id) : null);
	const featuredArtistHref = $derived(featured ? artistHrefOrNull(featured.album.artist_id) : null);
	const isFeaturedRequested = $derived(
		!!featured &&
			(featured.album.requested || libraryStore.isRequested(featured.album.musicbrainz_id))
	);

	// cycling is MANUAL only: a self-rotating hero yanks the card away mid-read
	function cycle(delta: number) {
		if (picks.length === 0) return;
		slideDirection = delta;
		featuredIndex = (featuredIndex + delta + picks.length) % picks.length;
	}

	function promote(index: number) {
		if (index === featuredIndex) return;
		slideDirection = index > featuredIndex ? 1 : -1;
		featuredIndex = index;
	}

	async function ignoreFeatured() {
		if (!featured?.album.musicbrainz_id || !featured.album.artist_id || !onignore || dismissing)
			return;
		dismissing = true;
		try {
			await onignore(featured);
			dismissed.add(featured.album.musicbrainz_id);
			featuredIndex = 0;
		} catch {
			return;
		} finally {
			dismissing = false;
		}
	}

	const RING_R = 26;
	const RING_C = 2 * Math.PI * RING_R;
</script>

{#if picks.length > 0 && featured}
	<section class="mb-6 sm:mb-8">
		<Card variant="bordered" class="relative overflow-hidden">
			<HeroBackdrop
				imageUrl={featured.album.musicbrainz_id
					? coverSrc('release-group', featured.album.musicbrainz_id, 500)
					: null}
				opacity={0.1}
				hoverOpacity={0.14}
				blur={26}
				hoverBlur={22}
				position="full"
			/>

			<div class="relative mb-2 flex items-center justify-between gap-3">
				<div class="flex items-center gap-2">
					<span class="animate-glow-pulse rounded-lg p-1">
						<Sparkles class="h-5 w-5 text-primary" />
					</span>
					<div>
						<div class="flex items-center gap-2">
							<h2 class="text-lg font-bold sm:text-xl">{section.title}</h2>
							<SourceBadge source={section.source ?? undefined} />
						</div>
						{#if section.personalizing}
							<div class="mt-0.5">
								<LiveUpdatingBadge label="Personalising your picks" />
							</div>
						{:else if updating}
							<div class="mt-0.5">
								<LiveUpdatingBadge label="Updating picks" className="px-2 py-0.5" />
							</div>
						{:else}
							<p class="text-xs text-base-content/50">Scored against your taste</p>
						{/if}
					</div>
				</div>
				{#if picks.length > 1}
					<div class="flex items-center gap-1">
						<IconButton
							size="sm"
							icon={ChevronLeft}
							label="Previous pick"
							onclick={() => cycle(-1)}
						/>
						<span class="font-mono text-xs text-base-content/40">
							{featuredIndex + 1}/{picks.length}
						</span>
						<IconButton size="sm" icon={ChevronRight} label="Next pick" onclick={() => cycle(1)} />
					</div>
				{/if}
			</div>

			{#key featured.album.musicbrainz_id}
				<div
					in:fly={{ x: 32 * slideDirection, duration: 250 }}
					class="relative flex flex-col gap-5 sm:flex-row sm:items-center"
				>
					<svelte:element
						this={featuredHref ? 'a' : 'div'}
						href={featuredHref ?? undefined}
						class="group/cover relative mx-auto w-44 shrink-0 overflow-hidden rounded-xl shadow-lg transition-transform duration-300 sm:mx-0 sm:w-52 {featuredHref
							? 'hover:scale-[1.02]'
							: ''}"
					>
						<AlbumImage
							mbid={featured.album.musicbrainz_id || ''}
							alt={featured.album.title}
							size="full"
							requestSize={250}
							lazy={false}
							rounded="none"
							className="block aspect-square w-full object-cover"
							customUrl={featured.album.cover_url || null}
						/>
					</svelte:element>

					<div
						class="flex min-w-0 flex-1 flex-col items-center gap-3 text-center sm:flex-row sm:gap-6 sm:text-left"
					>
						<!-- match ring -->
						<div class="relative h-24 w-24 shrink-0" title="{featured.match_pct}% match">
							<svg viewBox="0 0 64 64" class="h-full w-full -rotate-90">
								<circle
									cx="32"
									cy="32"
									r={RING_R}
									fill="none"
									class="stroke-base-content/10"
									stroke-width="5"
								/>
								<circle
									cx="32"
									cy="32"
									r={RING_R}
									fill="none"
									class="match-ring-arc stroke-primary"
									stroke-width="5"
									stroke-linecap="round"
									stroke-dasharray={RING_C}
									stroke-dashoffset={RING_C * (1 - featured.match_pct / 100)}
								/>
							</svg>
							<div class="absolute inset-0 flex flex-col items-center justify-center">
								<span class="text-xl font-extrabold leading-none">{featured.match_pct}%</span>
								<span class="text-[0.6rem] uppercase tracking-widest text-base-content/50"
									>match</span
								>
							</div>
						</div>

						<div class="min-w-0 flex-1">
							<svelte:element
								this={featuredHref ? 'a' : 'span'}
								href={featuredHref ?? undefined}
								class="block truncate text-xl font-extrabold tracking-tight sm:text-2xl {featuredHref
									? 'transition-colors hover:text-primary'
									: ''}"
							>
								{featured.album.title}
							</svelte:element>
							<svelte:element
								this={featuredArtistHref ? 'a' : 'span'}
								href={featuredArtistHref ?? undefined}
								class="text-sm font-semibold uppercase tracking-wide text-base-content/60 {featuredArtistHref
									? 'transition-colors hover:text-primary'
									: ''}"
							>
								{featured.album.artist_name}
							</svelte:element>

							{#if featured.reasons.length > 0}
								<div class="mt-2 flex flex-wrap justify-center gap-1.5 sm:justify-start">
									{#each featured.reasons as reason (reason)}
										<span class="badge badge-sm border-primary/20 bg-primary/10 text-primary/90">
											{reason}
										</span>
									{/each}
								</div>
							{/if}

							<div class="mt-3 flex items-center justify-center gap-2 sm:justify-start">
								{#if featured.album.musicbrainz_id && !featured.album.in_library && !isFeaturedRequested}
									<AlbumRequestButton
										mbid={featured.album.musicbrainz_id}
										artistName={featured.album.artist_name ?? ''}
										albumName={featured.album.title}
										artistMbid={featured.album.artist_id ?? undefined}
									/>
								{/if}
								{#if featured.album.musicbrainz_id}
									<SampleButton
										sampleKey={featured.album.musicbrainz_id}
										artist={featured.album.artist_name ?? ''}
										title={featured.album.title}
										kind="album"
										size="sm"
										artistMbid={featured.album.artist_id}
										coverUrl={featured.album.cover_url}
									/>
								{/if}
								{#if featured.album.artist_id}
									<RadioPlayButton
										seed={{ seed_type: 'artist', seed_id: featured.album.artist_id }}
										size="sm"
										variant="ghost"
										label="Start radio"
									/>
								{/if}
								{#if onignore && featured.album.musicbrainz_id && featured.album.artist_id}
									<Button
										variant="ghost"
										size="sm"
										icon={ThumbsDown}
										disabled={dismissing}
										title="Show fewer recommendations like this"
										onclick={() => void ignoreFeatured()}
									>
										<span class="hidden sm:inline">Not for me</span>
									</Button>
								{/if}
							</div>
						</div>
					</div>
				</div>
			{/key}

			{#if picks.length > 1}
				<div class="relative mt-3">
					<HorizontalCarousel>
						{#each picks as pick, i (pick.album.musicbrainz_id ?? `${pick.album.title}-${i}`)}
							<button
								class="relative w-20 shrink-0 overflow-hidden rounded-lg transition-all duration-200 {i ===
								featuredIndex
									? 'ring-2 ring-primary ring-offset-2 ring-offset-base-100'
									: 'opacity-60 hover:opacity-100'}"
								onclick={() => promote(i)}
								aria-label="Feature {pick.album.title}"
								title="{pick.album.title} - {pick.match_pct}% match"
							>
								<AlbumImage
									mbid={pick.album.musicbrainz_id || ''}
									alt={pick.album.title}
									size="full"
									requestSize={250}
									lazy={true}
									rounded="none"
									className="block aspect-square w-full object-cover"
									customUrl={pick.album.cover_url || null}
								/>
								<span
									class="absolute bottom-1 right-1 rounded-full bg-base-100/90 px-1.5 py-0.5 font-mono text-[0.6rem] font-bold text-primary shadow"
								>
									{pick.match_pct}%
								</span>
							</button>
						{/each}
					</HorizontalCarousel>
				</div>
			{/if}
		</Card>
	</section>
{/if}

<style>
	.match-ring-arc {
		transition: stroke-dashoffset 0.6s var(--ease-spring, ease-out);
	}

	@media (prefers-reduced-motion: reduce) {
		.match-ring-arc {
			transition: none;
		}
	}
</style>
