<script lang="ts">
	import { Download, Music, CircleAlert, TrendingUp, Sparkles, Library } from 'lucide-svelte';
	import HomeSection from '$lib/components/HomeSection.svelte';
	import WeeklyExploration from '$lib/components/WeeklyExploration.svelte';
	import ServicePromptCard from '$lib/components/ServicePromptCard.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import GenreGrid from '$lib/components/GenreGrid.svelte';
	import SectionDivider from '$lib/components/SectionDivider.svelte';
	import type {
		HomeSection as HomeSectionType,
		WeeklyExplorationSection as WeeklyExplorationSectionType
	} from '$lib/types';
	import { type MusicSource, isMusicSource } from '$lib/stores/musicSource';
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import { getGreeting } from '$lib/utils/homeCache';
	import { removeQueueCachedData } from '$lib/utils/discoverQueueCache';
	import { isDismissed } from '$lib/utils/dismissedPrompts';
	import HomeSectionNowPlaying from '$lib/components/HomeSectionNowPlaying.svelte';
	import HomeEntryCards from '$lib/components/HomeEntryCards.svelte';
	import { getHomeQuery } from '$lib/queries/HomeQuery.svelte';
	import type { PageProps } from './$types';
	import { PersistedState } from 'runed';
	import { PAGE_SOURCE_KEYS } from '$lib/constants';
	import SimpleSourceSwitcher from '$lib/components/SimpleSourceSwitcher.svelte';

	const { data }: PageProps = $props();

	// svelte-ignore state_referenced_locally
	let activeSource = new PersistedState<MusicSource>(PAGE_SOURCE_KEYS['home'], data.primarySource);

	let validSource = $derived(
		isMusicSource(activeSource.current) ? activeSource.current : data.primarySource
	);

	const homeQuery = getHomeQuery(() => validSource);
	const homeData = $derived(homeQuery.data);
	const loading = $derived(homeQuery.isLoading);
	const isUpdating = $derived(homeQuery.isRefetching);
	const lastUpdated = $derived(homeQuery.dataUpdatedAt ? new Date(homeQuery.dataUpdatedAt) : null);

	function handleSourceChange(source: MusicSource) {
		activeSource.current = source;
		removeQueueCachedData(authStore.user?.id ?? 'anon');
	}

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
		if (
			validSource === 'listenbrainz' &&
			homeData.weekly_exploration &&
			homeData.weekly_exploration.tracks.length > 0
		) {
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
		refreshing={isUpdating}
		{isUpdating}
		{lastUpdated}
		onRefresh={() => homeQuery.refetch()}
	>
		{#snippet title()}
			<Music class="inline h-8 w-8 sm:h-10 sm:w-10 lg:h-12 lg:w-12 mr-2 align-text-bottom" />
			{getGreeting()}
		{/snippet}
	</PageHeader>

	<div class="flex justify-end px-4 -mt-4 mb-4 sm:px-6 lg:px-8">
		<SimpleSourceSwitcher currentSource={validSource} onSourceChange={handleSourceChange} />
	</div>

	<div class="mb-10 px-4 sm:mb-12 sm:px-6 lg:px-8">
		<HomeEntryCards />
	</div>

	{#if homeQuery.error && !homeData}
		<div class="mt-16 flex flex-col items-center justify-center px-4">
			<CircleAlert class="mb-4 h-10 w-10 text-base-content/50" />
			<p class="text-base-content/70">{homeQuery.error.message ?? 'Failed to load Home data'}</p>
			<button class="btn btn-primary mt-4" onclick={() => homeQuery.refetch()}>Try Again</button>
		</div>
	{:else}
		<div class="space-y-10 px-4 sm:space-y-12 sm:px-6 lg:px-8">
			{#if !downloadClientConfigured && downloadClientPrompt}
				<div
					class="card bg-linear-to-br from-accent/20 via-accent/10 to-base-200 border-2 border-accent/40 shadow-xl relative overflow-hidden"
				>
					<div class="card-body items-center text-center py-12 stagger-fade-in">
						<Music class="h-16 w-16 mb-4 animate-float text-accent" />
						<h2 class="card-title text-3xl sm:text-4xl lg:text-5xl font-bold mb-2">
							Welcome to <span class="text-primary">DroppedNeedle</span>!
						</h2>
						<p class="text-base-content/70 max-w-lg mb-6">
							Get started by connecting a download client. You need it to request albums and tracks
							for your library.
						</p>
						<div class="flex flex-wrap justify-center gap-2 mb-6">
							{#each downloadClientPrompt.features as feature (feature)}
								<span class="badge badge-accent badge-lg">{feature}</span>
							{/each}
						</div>
						<a href="/settings?tab=download-client" class="btn btn-accent btn-lg gap-2">
							<Download class="h-5 w-5" />
							Configure Download Client
						</a>
					</div>
				</div>
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
							{#each whatsHotBlocks as block (block.key)}
								<div>
									{#if block.kind === 'section'}
										<HomeSection
											section={block.section}
											headerLink={block.link}
											showPreview={block.showPreview}
										/>
									{:else}
										<WeeklyExploration
											section={block.section}
											ytConfigured={homeData?.integration_status?.youtube ?? false}
										/>
									{/if}
								</div>
							{/each}
						</div>
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
										<WeeklyExploration
											section={block.section}
											ytConfigured={homeData?.integration_status?.youtube ?? false}
										/>
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
					<div class="grid grid-cols-2 gap-2 sm:grid-cols-3 sm:gap-3 md:grid-cols-4 lg:grid-cols-5">
						{#each Array(10) as _, i (`genre-skeleton-${i}`)}
							<div class="skeleton skeleton-shimmer h-20 rounded-lg sm:h-24"></div>
						{/each}
					</div>
				</section>
			{:else if homeData?.genre_list && homeData.genre_list.items.length > 0}
				<div class="mt-10 mb-10">
					<GenreGrid
						title={homeData.genre_list.title}
						genres={homeData.genre_list.items}
						genreArtists={homeData.genre_artists}
						genreArtistImages={homeData.genre_artist_images}
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
						{#each postGenreSections as { key, section, link } (key)}
							<div>
								<HomeSection {section} headerLink={link} />
							</div>
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
