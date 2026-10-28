<script lang="ts">
	import {
		Download,
		Music,
		CircleAlert,
		TrendingUp,
		Sparkles,
		Library,
		SlidersHorizontal
	} from 'lucide-svelte';
	import HomeSection from '$lib/components/HomeSection.svelte';
	import ServiceErrorState from '$lib/components/ServiceErrorState.svelte';
	import WeeklyExploration from '$lib/components/WeeklyExploration.svelte';
	import ServicePromptCard from '$lib/components/ServicePromptCard.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import GenreGrid from '$lib/components/GenreGrid.svelte';
	import SectionDivider from '$lib/components/SectionDivider.svelte';
	import type {
		ChartSection as HomeSectionType,
		SectionKey,
		SectionZone,
		WeeklyExploration as WeeklyExplorationSectionType
	} from '$lib/queries/discover/types';
	import { shelfOf, zoneSections } from '$lib/queries/discover/sections';
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { genreRows } from '$lib/utils/sectionRows';
	import { getGreeting } from '$lib/utils/homeCache';
	import { isDismissed } from '$lib/utils/dismissedPrompts';
	import HomeSectionNowPlaying from '$lib/components/HomeSectionNowPlaying.svelte';
	import HomeEntryCards from '$lib/components/HomeEntryCards.svelte';
	import DiscoverTeaserBand from '$lib/components/discover/DiscoverTeaserBand.svelte';
	import { getHomeQuery } from '$lib/queries/HomeQuery.svelte';
	import { useDiscoverActivity } from '$lib/queries/discover/DiscoverDemand.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';

	useDiscoverActivity(() => ({ feature: 'home' }));

	const homeQuery = getHomeQuery();
	const homeData = $derived(homeQuery.data);
	const loading = $derived(homeQuery.isLoading);
	const isUpdating = $derived(homeQuery.isRefetching);
	const lastUpdated = $derived(homeQuery.dataUpdatedAt ? new Date(homeQuery.dataUpdatedAt) : null);

	type Block =
		| {
				key: SectionKey;
				kind: 'section';
				section: HomeSectionType;
				link?: string;
				showPreview?: boolean;
		  }
		| { key: SectionKey; kind: 'weekly'; section: WeeklyExplorationSectionType };

	// where a shelf's header links to, and which shelves skip the hover preview
	const SHELF_LINKS: Partial<Record<SectionKey, { link: string; showPreview?: boolean }>> = {
		popular_albums: { link: '/popular' },
		trending_artists: { link: '/trending' },
		your_top_albums: { link: '/your-top', showPreview: false },
		recently_added: { link: '/library/albums', showPreview: false },
		library_artists: { link: '/library/artists' },
		library_albums: { link: '/library/albums' }
	};

	function blocks(zone: SectionZone): Block[] {
		return zoneSections(homeData?.sections, zone).flatMap((entry): Block[] => {
			const { content, key } = entry;
			if (content.shape === 'weekly') return [{ key, kind: 'weekly', section: content.weekly }];
			if (content.shape !== 'shelf') return [];
			const shelf = SHELF_LINKS[key];
			return [
				{
					key,
					kind: 'section',
					section: content.shelf,
					link: shelf ? withBasePath(shelf.link) : undefined,
					showPreview: shelf?.showPreview
				}
			];
		});
	}

	let whatsHotBlocks = $derived(blocks('whats_hot'));
	let forYouBlocks = $derived(blocks('for_you'));
	let genreList = $derived(shelfOf(homeData?.sections, 'genre_list'));
	let postGenreSections = $derived(blocks('your_library'));
	let hasContent = $derived((homeData?.sections.length ?? 0) > 0);
	let servicePrompts = $derived(homeData?.service_prompts || []);
	let downloadClientConfigured = $derived(homeData?.integration_status?.download_client ?? true);
	let downloadClientPrompt = $derived(servicePrompts.find((p) => p.service === 'download-client'));

	const getOtherPrompts = () => {
		return servicePrompts.filter((p) => p.service !== 'download-client' && !isDismissed(p.service));
	};
	let otherPrompts = $derived(getOtherPrompts());

	function handlePromptDismiss(_service: string) {
		otherPrompts = getOtherPrompts();
	}
</script>

<svelte:head>
	<title>Home - DroppedNeedle</title>
</svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHeader
		subtitle="Discover music, explore your library, and find new favorites."
		{loading}
		isUpdating={isUpdating || !!homeData?.refreshing}
		{lastUpdated}
	>
		{#snippet title()}
			<Music class="inline h-8 w-8 sm:h-10 sm:w-10 lg:h-12 lg:w-12 mr-2 align-text-bottom" />
			{getGreeting()}
		{/snippet}
	</PageHeader>

	<div class="flex justify-end px-4 -mt-4 mb-4 sm:px-6 lg:px-8">
		<Button
			variant="ghost"
			size="sm"
			icon={SlidersHorizontal}
			href={withBasePath('/settings?tab=home')}
			title="Choose which sections appear here"
		>
			<span class="hidden sm:inline">Customise</span>
		</Button>
	</div>

	<div class="mb-10 px-4 sm:mb-12 sm:px-6 lg:px-8">
		<HomeEntryCards />
	</div>

	{#if homeQuery.error && !homeData}
		<ServiceErrorState error={homeQuery.error} what="Home" onretry={() => homeQuery.refetch()} />
	{:else}
		<!-- a render error in any section degrades to the failed card below instead
		     of crashing the whole SPA (the #147 freeze) -->
		<svelte:boundary>
			<div
				class="space-y-10 px-4 sm:space-y-12 sm:px-6 lg:px-8"
				class:is-refreshing={homeData?.refreshing}
			>
				{#if !downloadClientConfigured && downloadClientPrompt}
					<Card
						class="relative overflow-hidden border-2 border-accent/40 bg-linear-to-br from-accent/20 via-accent/10 to-base-200 shadow-xl"
					>
						<div class="flex flex-col items-center gap-2 py-6 text-center stagger-fade-in">
							<Music class="h-16 w-16 mb-4 animate-float text-accent" />
							<CardTitle class="block text-2xl sm:text-4xl lg:text-5xl font-bold mb-2">
								Welcome to <span class="text-primary">DroppedNeedle</span>!
							</CardTitle>
							<p class="text-base-content/70 max-w-lg mb-6">
								Connect a download source to fetch your requests. Until then, requests wait.
							</p>
							<div class="flex flex-wrap justify-center gap-2 mb-6">
								{#each downloadClientPrompt.features as feature (feature)}
									<span class="badge badge-accent badge-lg">{feature}</span>
								{/each}
							</div>
							<Button
								variant="accent"
								size="lg"
								icon={Download}
								href={withBasePath('/settings?tab=download-client')}
							>
								Configure Download Client
							</Button>
						</div>
					</Card>
				{/if}

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
								{#each whatsHotBlocks as block, i (block.key)}
									<div>
										{#if block.kind === 'section'}
											<HomeSection
												section={block.section}
												headerLink={block.link}
												showPreview={block.showPreview}
												eager={i < 2}
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

				{#if loading && !homeData}
					<section>
						<div class="skeleton skeleton-shimmer mb-4 h-6 w-36"></div>
						<div
							class="grid grid-cols-2 gap-2 sm:grid-cols-3 sm:gap-3 md:grid-cols-4 lg:grid-cols-5"
						>
							{#each Array(10) as _, i (`genre-skeleton-${i}`)}
								<div class="skeleton skeleton-shimmer h-20 rounded-lg sm:h-24"></div>
							{/each}
						</div>
					</section>
				{:else if genreList}
					<div class="mt-10 mb-10">
						<GenreGrid
							title={genreList.title}
							genres={genreRows(genreList)}
							genreArtwork={homeData?.genre_artwork}
						/>
					</div>
				{/if}

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
							{#each postGenreSections as block (block.key)}
								{#if block.kind === 'section'}
									<div>
										<HomeSection section={block.section} headerLink={block.link} />
									</div>
								{/if}
							{/each}
						</div>
					</div>
				{/if}

				{#if !loading && !hasContent && servicePrompts.length === 0}
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
								<Button variant="primary" href={withBasePath('/settings?tab=library')}>
									Start scan
								</Button>
								<Button variant="ghost" href={withBasePath('/library')}>Go to Library</Button>
							</div>
						{:else}
							<p class="mb-6 max-w-md px-4 text-center text-sm text-base-content/70 sm:text-base">
								Your library is being prepared. An admin is setting things up - check back soon.
							</p>
							<Button variant="ghost" href={withBasePath('/library')}>Go to Library</Button>
						{/if}
					</div>
				{/if}
			</div>

			{#snippet failed(_error, reset)}
				<div class="flex flex-col items-center justify-center py-12 sm:py-16">
					<CircleAlert class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-base-content/50" />
					<h2 class="mb-2 text-center text-xl font-bold sm:text-2xl">Something Went Wrong</h2>
					<p class="mb-6 max-w-md px-4 text-center text-sm text-base-content/70 sm:text-base">
						Part of the Home page failed to render. The rest of the app still works.
					</p>
					<Button variant="primary" onclick={() => reset()}>Try Again</Button>
				</div>
			{/snippet}
		</svelte:boundary>
	{/if}
</div>
