<script lang="ts">
	import {
		Download,
		Music,
		CircleAlert,
		TrendingUp,
		Sparkles,
		Library,
		ChevronRight
	} from 'lucide-svelte';
	import HomeSection from '$lib/components/HomeSection.svelte';
	import WeeklyExploration from '$lib/components/WeeklyExploration.svelte';
	import ServicePromptCard from '$lib/components/ServicePromptCard.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import SectionDivider from '$lib/components/SectionDivider.svelte';
	import type {
		HomeSection as HomeSectionType,
		WeeklyExplorationSection as WeeklyExplorationSectionType
	} from '$lib/types';
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import ExploreSpotlight from '$lib/components/home/ExploreSpotlight.svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import { getGreeting } from '$lib/utils/homeCache';
	import { isDismissed } from '$lib/utils/dismissedPrompts';
	import type { HomeAlbum } from '$lib/types';
	import HomeSectionNowPlaying from '$lib/components/HomeSectionNowPlaying.svelte';
	import HomeEntryCards from '$lib/components/HomeEntryCards.svelte';
	import DiscoverTeaserBand from '$lib/components/discover/DiscoverTeaserBand.svelte';
	import { getHomeQuery } from '$lib/queries/HomeQuery.svelte';

	const homeQuery = getHomeQuery();
	const homeData = $derived(homeQuery.data);
	const loading = $derived(homeQuery.isLoading);
	const isUpdating = $derived(homeQuery.isRefetching);
	const lastUpdated = $derived(homeQuery.dataUpdatedAt ? new Date(homeQuery.dataUpdatedAt) : null);

	type PreGenreBlock =
		| {
				key: string;
				kind: 'section';
				section: HomeSectionType;
				link?: string;
				showPreview?: boolean;
		  }
		| { key: 'weekly_exploration'; kind: 'weekly'; section: WeeklyExplorationSectionType };

	function getPreGenreBlocks(): PreGenreBlock[] {
		if (!homeData) return [];
		const blocks: PreGenreBlock[] = [];
		if (homeData.popular_albums && homeData.popular_albums.items.length > 0) {
			blocks.push({
				key: 'popular_albums',
				kind: 'section',
				section: homeData.popular_albums,
				link: '/popular'
			});
		}
		if (homeData.trending_artists && homeData.trending_artists.items.length > 0) {
			blocks.push({
				key: 'trending_artists',
				kind: 'section',
				section: homeData.trending_artists,
				link: '/trending'
			});
		}
		if (homeData.weekly_exploration && homeData.weekly_exploration.tracks.length > 0) {
			blocks.push({
				key: 'weekly_exploration',
				kind: 'weekly',
				section: homeData.weekly_exploration
			});
		}
		if (homeData.your_top_albums && homeData.your_top_albums.items.length > 0) {
			blocks.push({
				key: 'your_top_albums',
				kind: 'section',
				section: homeData.your_top_albums,
				link: '/your-top',
				showPreview: false
			});
		}
		if (homeData.recently_played && homeData.recently_played.items.length > 0) {
			blocks.push({
				key: 'recently_played',
				kind: 'section',
				section: homeData.recently_played
			});
		}
		if (homeData.recently_added && homeData.recently_added.items.length > 0) {
			blocks.push({
				key: 'recently_added',
				kind: 'section',
				section: homeData.recently_added,
				link: '/library/albums',
				showPreview: false
			});
		}
		return blocks;
	}

	function getPostGenreSections(): { key: string; section: HomeSectionType; link?: string }[] {
		if (!homeData) return [];
		const sections: { key: string; section: HomeSectionType; link?: string }[] = [];
		if (homeData.favorite_artists && homeData.favorite_artists.items.length > 0) {
			sections.push({
				key: 'favorite_artists',
				section: homeData.favorite_artists
			});
		}
		if (homeData.library_artists && homeData.library_artists.items.length > 0) {
			sections.push({
				key: 'library_artists',
				section: homeData.library_artists,
				link: '/library/artists'
			});
		}
		if (homeData.library_albums && homeData.library_albums.items.length > 0) {
			sections.push({
				key: 'library_albums',
				section: homeData.library_albums,
				link: '/library/albums'
			});
		}
		return sections;
	}
	let preGenreBlocks = $derived(homeData ? getPreGenreBlocks() : []);
	let postGenreSections = $derived(homeData ? getPostGenreSections() : []);

	const whatsHotKeys = new Set(['popular_albums', 'trending_artists']);
	let whatsHotBlocks = $derived(preGenreBlocks.filter((b) => whatsHotKeys.has(b.key)));
	let forYouBlocks = $derived(preGenreBlocks.filter((b) => !whatsHotKeys.has(b.key)));
	let hasContent = $derived(
		preGenreBlocks.length > 0 ||
			postGenreSections.length > 0 ||
			(homeData?.genre_list?.items?.length ?? 0) > 0
	);
	let servicePrompts = $derived(homeData?.service_prompts || []);
	let downloadClientConfigured = $derived(homeData?.integration_status?.download_client ?? true);
	let downloadClientPrompt = $derived(servicePrompts.find((p) => p.service === 'download-client'));

	// Cinematic spotlight — YOUR music first. Global charts (ListenBrainz) are
	// only the cold-start fallback for brand-new instances with no history.
	const spotlightPersonal = $derived.by(() => {
		const pools = [
			homeData?.recently_played?.items ?? [],
			homeData?.your_top_albums?.items ?? [],
			homeData?.recently_added?.items ?? [],
			homeData?.library_albums?.items ?? []
		];
		const seen = new SvelteSet<string>();
		const picks: HomeAlbum[] = [];
		for (const pool of pools) {
			for (const raw of pool) {
				const item = raw as HomeAlbum;
				if (!item.mbid || !item.artist_mbid || seen.has(item.mbid)) continue;
				seen.add(item.mbid);
				picks.push(item);
				if (picks.length >= 5) return picks;
			}
		}
		return picks;
	});

	const spotlightItems = $derived.by(() => {
		if (spotlightPersonal.length > 0) return spotlightPersonal;
		const pool = (homeData?.popular_albums?.items ?? []) as HomeAlbum[];
		const seen = new SvelteSet<string>();
		const picks: HomeAlbum[] = [];
		for (const item of pool) {
			if (!item.mbid || !item.artist_mbid || seen.has(item.mbid)) continue;
			seen.add(item.mbid);
			picks.push(item);
			if (picks.length >= 5) break;
		}
		return picks;
	});

	const spotlightEyebrow = $derived(
		spotlightPersonal.length > 0 ? 'From your rotation' : 'Trending this week'
	);

	const getOtherPrompts = () => {
		return servicePrompts.filter((p) => p.service !== 'download-client' && !isDismissed(p.service));
	};
	let otherPrompts = $derived(getOtherPrompts());

	function handlePromptDismiss(_service: string) {
		otherPrompts = getOtherPrompts();
	}
</script>

<svelte:head>
	<title>Explore - DroppedNeedle</title>
</svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	{#if spotlightItems.length > 0}
		<ExploreSpotlight items={spotlightItems} eyebrow={spotlightEyebrow} />
	{:else if loading && !homeData}
		<div class="flex min-h-[40vh] items-end px-4 pb-10 sm:px-6 lg:px-8">
			<div class="w-full max-w-xl space-y-4">
				<div class="skeleton skeleton-shimmer h-6 w-36 rounded-full"></div>
				<div class="skeleton skeleton-shimmer h-16 w-4/5"></div>
				<div class="skeleton skeleton-shimmer h-5 w-2/3"></div>
			</div>
		</div>
	{/if}

	<!-- onboarding first: when the download client isn't wired up yet, the welcome
	     card owns the top of the page -->
	{#if homeData && !downloadClientConfigured && downloadClientPrompt}
		<!-- phones get one quiet line instead of the onboarding boxes -->
		<div class="px-4 pt-4 md:hidden">
			<a
				href="/settings?tab=download-client"
				class="flex items-center gap-3 rounded-2xl border border-base-content/10 bg-base-200/60 px-4 py-3"
			>
				<Download class="h-4 w-4 shrink-0 text-accent" />
				<span class="min-w-0 flex-1 text-sm text-base-content/75">
					Finish setup — connect a download client
				</span>
				<ChevronRight class="h-4 w-4 shrink-0 opacity-40" />
			</a>
		</div>

		<div class="hidden px-4 pt-6 sm:px-6 md:block lg:px-8">
			<div class="dn-welcome">
				<div class="dn-welcome__wash" aria-hidden="true"></div>
				<p
					class="font-mono text-[0.62rem] font-bold uppercase tracking-[0.22em] text-base-content/50"
				>
					First spin
				</p>
				<h2 class="hero-title mt-2 font-display text-3xl font-bold tracking-tight sm:text-4xl">
					Welcome to DroppedNeedle
				</h2>
				<p class="mt-3 max-w-lg text-base-content/65">
					Connect a download client to start requesting albums and tracks for your library.
				</p>
				<div class="mt-4 flex flex-wrap gap-1.5">
					{#each downloadClientPrompt.features as feature (feature)}
						<span
							class="rounded-full border border-base-content/10 bg-base-content/5 px-3 py-1 font-mono text-[0.6rem] font-bold uppercase tracking-[0.1em] text-base-content/60"
							>{feature}</span
						>
					{/each}
				</div>
				<a
					href="/settings?tab=download-client"
					class="btn btn-primary mt-6 gap-2 rounded-full shadow-lg"
				>
					<Download class="h-4 w-4" />
					Configure Download Client
				</a>
			</div>
		</div>
	{/if}

	<!-- the two big doors: desktop only — on phones the drawer + bottom bar cover them -->
	<div class="hidden px-4 pt-6 sm:px-6 md:block lg:px-8">
		<div class="flex items-baseline justify-between gap-3 pb-4">
			<p class="font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/45">
				{getGreeting()} · your setup
			</p>
			<p class="min-h-4 text-xs text-base-content/40">
				{#if isUpdating || homeData?.refreshing}
					<span class="inline-flex items-center gap-1.5">
						<span class="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-primary"></span>
						Updating…
					</span>
				{:else if lastUpdated}
					Updated {lastUpdated.toLocaleTimeString(undefined, {
						hour: 'numeric',
						minute: '2-digit'
					})}
				{/if}
			</p>
		</div>
		<div class="dn-entry-scale mb-12">
			<HomeEntryCards />
		</div>
	</div>

	{#if homeQuery.error && !homeData}
		<div class="mt-16 flex flex-col items-center justify-center px-4">
			<CircleAlert class="mb-4 h-10 w-10 text-base-content/50" />
			<p class="text-base-content/70">{homeQuery.error.message ?? 'Failed to load Home data'}</p>
			<button class="btn btn-primary mt-4" onclick={() => homeQuery.refetch()}>Try Again</button>
		</div>
	{:else}
		<div
			class="space-y-10 px-4 sm:space-y-12 sm:px-6 lg:px-8"
			class:is-refreshing={homeData?.refreshing}
		>
			{#if otherPrompts.length > 0 && downloadClientConfigured}
				<div class="space-y-3">
					{#each otherPrompts as prompt, i (`prompt-${i}`)}
						<ServicePromptCard {prompt} ondismiss={handlePromptDismiss} />
					{/each}
				</div>
			{/if}

			<HomeSectionNowPlaying />

			{#if loading && !homeData}
				<section>
					<div class="skeleton skeleton-shimmer mb-4 h-6 w-40"></div>
					<CarouselSkeleton />
				</section>
			{:else}
				{#if whatsHotBlocks.length > 0}
					<div>
						<SectionDivider label="What's Hot">
							{#snippet icon()}<TrendingUp class="w-3.5 h-3.5" />{/snippet}
						</SectionDivider>
						<div class="discover-section-enter space-y-2">
							{#each whatsHotBlocks as block (block.key)}
								<div>
									{#if block.kind === 'section'}
										<HomeSection
											section={block.section}
											headerLink={block.link}
											showPreview={block.showPreview}
										/>
									{:else}
										<WeeklyExploration section={block.section} />
									{/if}
								</div>
							{/each}
						</div>
					</div>
				{/if}

				{#if !loading || homeData}
					<div class="discover-section-enter">
						<DiscoverTeaserBand preview={homeData?.discover_preview ?? null} />
					</div>
				{/if}

				{#if forYouBlocks.length > 0}
					<div>
						<SectionDivider label="For You">
							{#snippet icon()}<Sparkles class="w-3.5 h-3.5" />{/snippet}
						</SectionDivider>
						<div class="discover-section-enter space-y-2">
							{#each forYouBlocks as block (block.key)}
								<div>
									{#if block.kind === 'section'}
										<HomeSection
											section={block.section}
											headerLink={block.link}
											showPreview={block.showPreview}
										/>
									{:else}
										<WeeklyExploration section={block.section} />
									{/if}
								</div>
							{/each}
						</div>
					</div>
				{/if}
			{/if}

			<!-- genre browsing lives in Discovery and Stations now; Home stays music-first -->

			{#if loading && !homeData}
				{#each Array(4) as _, i (`post-genre-skeleton-${i}`)}
					<section>
						<div class="skeleton skeleton-shimmer mb-4 h-6 w-32"></div>
						<CarouselSkeleton showSubtitle={false} />
					</section>
				{/each}
			{:else if postGenreSections.length > 0}
				<div>
					<SectionDivider label="Your Library">
						{#snippet icon()}<Library class="w-3.5 h-3.5" />{/snippet}
					</SectionDivider>
					<div class="discover-section-enter space-y-2">
						{#each postGenreSections as { key, section, link } (key)}
							<div>
								<HomeSection {section} headerLink={link} />
							</div>
						{/each}
					</div>
				</div>
			{/if}

			{#if !loading && !hasContent && servicePrompts.length === 0 && downloadClientConfigured}
				<div class="flex flex-col items-center justify-center py-12 sm:py-16">
					<Music class="h-12 w-12 sm:h-16 sm:w-16 mb-4 sm:mb-6" />
					<h2 class="mb-2 text-center text-3xl font-bold sm:text-4xl lg:text-5xl">
						Welcome to <span class="text-primary">DroppedNeedle</span>
					</h2>
					{#if authStore.isAdmin}
						<p class="mb-6 max-w-md px-4 text-center text-sm text-base-content/70 sm:text-base">
							Your library is empty. Add a library path and start a scan to fill it.
						</p>
						<div class="flex flex-wrap justify-center gap-2">
							<a href="/settings?tab=library" class="btn btn-primary">Start scan</a>
							<a href="/library" class="btn btn-ghost">Go to Library</a>
						</div>
					{:else}
						<p class="mb-6 max-w-md px-4 text-center text-sm text-base-content/70 sm:text-base">
							Your library is being prepared. An admin is setting things up - check back soon.
						</p>
						<a href="/library" class="btn btn-ghost">Go to Library</a>
					{/if}
				</div>
			{/if}
		</div>
	{/if}
</div>

<style>
	.dn-welcome {
		position: relative;
		isolation: isolate;
		overflow: hidden;
		padding: 2.5rem 2rem 2.75rem;
		border-radius: 1.5rem;
		border: 1px solid var(--dn-hairline);
	}
	.dn-welcome__wash {
		position: absolute;
		inset: 0;
		z-index: -1;
		background:
			radial-gradient(
				circle at 12% -30%,
				oklch(from var(--color-accent) l c h / 0.16),
				transparent 55%
			),
			radial-gradient(
				circle at 90% 130%,
				oklch(from var(--color-primary) l c h / 0.08),
				transparent 55%
			),
			oklch(from var(--color-base-200) l c h / 0.5);
	}

	/* the entry doors: prominent but not oversized — a notch above stock size */
	.dn-entry-scale :global(.card-body) {
		padding: 1.5rem;
		gap: 1rem;
	}
	.dn-entry-scale :global(.card-body h2) {
		font-size: 1.3rem;
	}
	@media (min-width: 1024px) {
		.dn-entry-scale :global(.card-body) {
			padding: 1.75rem;
		}
		.dn-entry-scale :global(.card-body h2) {
			font-size: 1.45rem;
		}
	}
</style>
