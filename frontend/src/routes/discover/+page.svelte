<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import HomeSection from '$lib/components/HomeSection.svelte';
	import DailyMixCard from '$lib/components/DailyMixCard.svelte';
	import RadioCard from '$lib/components/RadioCard.svelte';
	import DiscoverPicksSection from '$lib/components/DiscoverPicksSection.svelte';
	import GenrePills from '$lib/components/GenrePills.svelte';
	import GenreGrid from '$lib/components/GenreGrid.svelte';
	import DiscoverQueueCard from '$lib/components/DiscoverQueueCard.svelte';
	import DiscoverQueueModal from '$lib/components/DiscoverQueueModal.svelte';
	import PlaylistDiscoveryModal from '$lib/components/PlaylistDiscoveryModal.svelte';
	import WeeklyExploration from '$lib/components/WeeklyExploration.svelte';
	import ServicePromptCard from '$lib/components/ServicePromptCard.svelte';
	import SourceSwitcher from '$lib/components/SourceSwitcher.svelte';
	import DiscoverArtistHero from '$lib/components/DiscoverArtistHero.svelte';
	import DiscoverArtistMiniBand from '$lib/components/DiscoverArtistMiniBand.svelte';
	import SectionDivider from '$lib/components/SectionDivider.svelte';
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import { removeAllQueueCachedData } from '$lib/utils/discoverQueueCache';
	import { api } from '$lib/api/client';
	import { isDismissed } from '$lib/utils/dismissedPrompts';
	import { musicSourceStore, type MusicSource } from '$lib/stores/musicSource';
	import { discoverQueueStatusStore } from '$lib/stores/discoverQueueStatus';
	import {
		Compass,
		CircleAlert,
		Sparkles,
		Music,
		Music2,
		Radio,
		Library,
		TrendingUp,
		LayoutGrid,
		Wand2,
		Heart
	} from 'lucide-svelte';
	import { getDiscoverQuery } from '$lib/queries/discover/DiscoverQuery.svelte';
	import { discoverHasContent } from '$lib/utils/discoverContent';
	import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { API } from '$lib/constants';

	let queueModalOpen = $state(false);
	let playlistDiscoverOpen = $state(false);
	let activeSource: MusicSource = $state('listenbrainz');

	const discoverQuery = getDiscoverQuery(() => activeSource);
	const discoverData = $derived(discoverQuery.data ?? null);
	const loading = $derived(discoverQuery.isLoading);
	const refreshing = $derived(discoverQuery.isFetching && !discoverQuery.isLoading);
	const isUpdating = $derived(discoverQuery.isRefetching && !!discoverData);
	const lastUpdated = $derived(
		discoverQuery.dataUpdatedAt ? new Date(discoverQuery.dataUpdatedAt) : null
	);
	const error = $derived(discoverQuery.error?.message ?? '');

	async function handleRefresh() {
		await api.global.post(API.discoverRefresh(activeSource));
		await invalidateQueriesWithPersister({
			queryKey: DiscoverQueryKeyFactory.discover(authStore.user?.id, activeSource)
		});
	}

	function handleSourceChange(source: MusicSource) {
		activeSource = source;
		removeAllQueueCachedData();
		discoverQueueStatusStore.reset();
		discoverQueueStatusStore.init(source);
	}

	onMount(async () => {
		await musicSourceStore.load();
		activeSource = musicSourceStore.getPageSource('discover');
		discoverQueueStatusStore.init(activeSource);
	});

	onDestroy(() => {
		discoverQueueStatusStore.stopPolling();
	});

	let hasContent = $derived(discoverHasContent(discoverData));
	let isBuilding = $derived(
		!!discoverData && !hasContent && (!!discoverData.refreshing || isUpdating)
	);
	let isUpdatingInBackground = $derived(!!discoverData?.refreshing && hasContent);
	let dismissVersion = $state(0);
	let servicePrompts = $derived.by(() => {
		void dismissVersion;
		return (discoverData?.service_prompts ?? []).filter((p) => !isDismissed(p.service));
	});

	let hasWeeklyExploration = $derived(
		activeSource === 'listenbrainz' &&
			!!discoverData?.weekly_exploration &&
			discoverData.weekly_exploration.tracks.length > 0
	);

	let queueIsHero = $derived(!hasWeeklyExploration && !!discoverData?.discover_queue_enabled);

	let hasHero = $derived(hasWeeklyExploration || queueIsHero);
	let showQueueInQuickActions = $derived(!!discoverData?.discover_queue_enabled && !queueIsHero);

	let hasMadeForYou = $derived(
		(discoverData?.daily_mixes?.length ?? 0) > 0 || (discoverData?.radio_sections?.length ?? 0) > 0
	);

	let hasBecauseYouListened = $derived(
		(discoverData?.because_you_listen_to?.length ?? 0) > 0 ||
			(discoverData?.artists_you_might_like?.items?.length ?? 0) > 0 ||
			(discoverData?.popular_in_your_genres?.items?.length ?? 0) > 0
	);

	let hasNewFresh = $derived(
		(discoverData?.fresh_releases?.items?.length ?? 0) > 0 ||
			(discoverData?.discover_picks?.items?.length ?? 0) > 0 ||
			(discoverData?.missing_essentials?.items?.length ?? 0) > 0
	);

	let hasFromYourLibrary = $derived(
		(discoverData?.rediscover?.items?.length ?? 0) > 0 ||
			(discoverData?.lastfm_recent_scrobbles?.items?.length ?? 0) > 0
	);

	let hasBrowseGenres = $derived(
		(discoverData?.unexplored_genres?.items?.length ?? 0) > 0 ||
			(discoverData?.genre_list?.items?.length ?? 0) > 0
	);

	let hasTrending = $derived(
		(discoverData?.globally_trending?.items?.length ?? 0) > 0 ||
			(discoverData?.lastfm_weekly_artist_chart?.items?.length ?? 0) > 0 ||
			(discoverData?.lastfm_weekly_album_chart?.items?.length ?? 0) > 0
	);

	let shuffledGenres = $state<
		{ name: string; listen_count?: number | null; artist_count?: number | null }[]
	>([]);
	$effect(() => {
		const items = discoverData?.unexplored_genres?.items;
		if (items && items.length > 0) {
			shuffledGenres = [...(items as typeof shuffledGenres)];
		} else {
			shuffledGenres = [];
		}
	});

	function shuffleGenres() {
		const copy = [...shuffledGenres];
		for (let i = copy.length - 1; i > 0; i--) {
			const j = Math.floor(Math.random() * (i + 1));
			[copy[i], copy[j]] = [copy[j], copy[i]];
		}
		shuffledGenres = copy;
	}

	function handlePromptDismiss(_service: string) {
		dismissVersion++;
	}
</script>

<svelte:head>
	<title>Discover - DroppedNeedle</title>
</svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHeader
		subtitle="Music recommendations based on what you listen to."
		gradientClass="bg-gradient-to-br from-info/30 via-primary/20 to-secondary/10"
		{loading}
		{refreshing}
		{isUpdating}
		{lastUpdated}
		refreshLabel="Refresh"
		onRefresh={() => handleRefresh()}
	>
		{#snippet title()}
			<Compass class="inline h-8 w-8 sm:h-10 sm:w-10 lg:h-12 lg:w-12 mr-2 align-text-bottom" />
			Discover
		{/snippet}
	</PageHeader>

	<div class="flex justify-end px-4 -mt-4 mb-4 sm:px-6 lg:px-8">
		<SourceSwitcher pageKey="discover" onSourceChange={handleSourceChange} />
	</div>

	{#if error && !discoverData}
		<div class="mt-16 flex flex-col items-center justify-center px-4">
			<CircleAlert class="mb-4 h-10 w-10 text-base-content/50" />
			<p class="text-base-content/70">{error}</p>
			<button class="btn btn-primary mt-4" onclick={() => discoverQuery.refetch()}>Try Again</button
			>
		</div>
	{:else}
		<div class="px-4 sm:px-6 lg:px-8">
			{#if servicePrompts.length > 0}
				<div class="space-y-3 mb-6">
					{#each servicePrompts as prompt, i (`service-prompt-${prompt.service}-${i}`)}
						<ServicePromptCard {prompt} ondismiss={handlePromptDismiss} />
					{/each}
				</div>
			{/if}

			{#if (loading && !discoverData) || isBuilding}
				{#if isBuilding}
					<div class="mb-8 flex items-center justify-center gap-3 px-4 text-center">
						<span class="loading loading-spinner loading-sm text-primary"></span>
						<p class="text-sm text-base-content/60">
							Building your recommendations from your listening history. The first load can take a
							moment.
						</p>
					</div>
				{/if}
				<div class="space-y-8">
					{#each Array(3) as _, i (`loading-section-${i}`)}
						<section>
							<div class="skeleton skeleton-shimmer mb-4 h-6 w-48"></div>
							<CarouselSkeleton />
						</section>
					{/each}
				</div>
			{:else if discoverData}
				<div class="space-y-10 sm:space-y-12">
					{#if isUpdatingInBackground}
						<div class="flex items-center justify-center gap-2 pb-1 text-xs text-base-content/50">
							<span class="loading loading-spinner loading-xs text-primary"></span>
							Updating your recommendations…
						</div>
					{/if}
					{#if hasHero}
						<div class="discover-section-enter">
							{#if hasWeeklyExploration && discoverData.weekly_exploration}
								<WeeklyExploration
									section={discoverData.weekly_exploration}
									ytConfigured={discoverData.integration_status?.youtube ?? false}
								/>
							{:else if queueIsHero}
								<DiscoverQueueCard source={activeSource} onLaunch={() => (queueModalOpen = true)} />
							{/if}
						</div>
					{/if}

					<div class="discover-section-enter">
						<div class="grid grid-cols-1 gap-4 {showQueueInQuickActions ? 'md:grid-cols-2' : ''}">
							{#if showQueueInQuickActions}
								<DiscoverQueueCard source={activeSource} onLaunch={() => (queueModalOpen = true)} />
							{/if}
							<button
								type="button"
								class="group relative w-full overflow-hidden rounded-2xl border border-primary/15 bg-gradient-to-br from-primary/8 via-base-200/50 to-secondary/8 px-5 py-7 backdrop-blur-sm shadow-[0_4px_24px_oklch(from_var(--color-primary)_l_c_h_/_0.06)] transition-all duration-300 cursor-pointer text-left motion-safe:hover:-translate-y-0.5 hover:shadow-[0_8px_32px_oklch(from_var(--color-primary)_l_c_h_/_0.15)] hover:border-primary/25 active:scale-[0.98] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100"
								onclick={() => (playlistDiscoverOpen = true)}
							>
								<div
									class="pointer-events-none absolute inset-0 rounded-2xl bg-gradient-to-br from-white/[0.03] to-transparent"
								></div>
								<div class="flex items-center gap-4">
									<div
										class="flex h-11 w-11 shrink-0 items-center justify-center rounded-xl bg-primary/15 shadow-[0_0_16px_oklch(from_var(--color-primary)_l_c_h_/_0.15)]"
									>
										<Wand2 class="h-5 w-5 text-primary" />
									</div>
									<div class="flex-1 min-w-0">
										<h3 class="font-bold text-sm sm:text-base">Discover for a Playlist</h3>
										<p class="text-xs text-base-content/50 mt-0.5">
											Get album suggestions based on any playlist.
										</p>
									</div>
									<div
										class="shrink-0 text-primary/50 transition-transform duration-300 group-hover:translate-x-1"
									>
										<Sparkles class="h-5 w-5" />
									</div>
								</div>
							</button>
						</div>
					</div>

					{#if hasMadeForYou}
						<div>
							<SectionDivider label="Made For You">
								{#snippet icon()}<Sparkles class="w-3.5 h-3.5" />{/snippet}
							</SectionDivider>

							<div class="discover-section-enter space-y-8 sm:space-y-10">
								{#if discoverData.daily_mixes?.length}
									<div>
										<h3
											class="text-sm font-semibold text-base-content/70 mb-3 flex items-center gap-2"
										>
											<Music2 class="h-4 w-4" />
											Daily Mixes
										</h3>
										<div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
											{#each discoverData.daily_mixes as mix, i (`${mix.title}-${i}`)}
												<DailyMixCard section={mix} />
											{/each}
										</div>
									</div>
								{/if}

								{#if discoverData.radio_sections?.length}
									<div>
										<h3
											class="text-sm font-semibold text-base-content/70 mb-3 flex items-center gap-2"
										>
											<Radio class="h-4 w-4" />
											Radio Stations
										</h3>
										<div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
											{#each discoverData.radio_sections as radio (`radio-${radio.radio_seed_id}`)}
												<RadioCard
													seedType={radio.radio_seed_type ?? 'artist'}
													seedId={radio.radio_seed_id ?? ''}
													source={activeSource}
													initialSection={radio}
												/>
											{/each}
										</div>
									</div>
								{/if}
							</div>
						</div>
					{/if}

					<div>
						<SectionDivider label="Because You Listened">
							{#snippet icon()}<Heart class="w-3.5 h-3.5" />{/snippet}
						</SectionDivider>

						{#if hasBecauseYouListened}
							<div class="discover-section-enter space-y-5 sm:space-y-6">
								{#each discoverData.because_you_listen_to as entry, i (entry.seed_artist_mbid || entry.seed_artist)}
									<div>
										{#if i === 0}
											<DiscoverArtistHero {entry} />
										{:else}
											<DiscoverArtistMiniBand {entry} />
										{/if}
									</div>
								{/each}

								{#if discoverData.artists_you_might_like && discoverData.artists_you_might_like.items.length > 0}
									<div><HomeSection section={discoverData.artists_you_might_like} /></div>
								{/if}

								{#if discoverData.popular_in_your_genres && discoverData.popular_in_your_genres.items.length > 0}
									<div><HomeSection section={discoverData.popular_in_your_genres} /></div>
								{/if}
							</div>
						{:else}
							<EmptyState
								icon={Heart}
								title="Listen to more music to see recommendations"
								description="As you build up your listening history, we'll surface artists and albums based on what you love."
							/>
						{/if}
					</div>

					{#if hasNewFresh}
						<div>
							<SectionDivider label="New & Fresh">
								{#snippet icon()}<Music class="w-3.5 h-3.5" />{/snippet}
							</SectionDivider>

							<div class="discover-section-enter space-y-2">
								{#if discoverData.fresh_releases && discoverData.fresh_releases.items.length > 0}
									<HomeSection section={discoverData.fresh_releases} />
								{/if}

								{#if discoverData.discover_picks && discoverData.discover_picks.items.length > 0}
									<DiscoverPicksSection section={discoverData.discover_picks} />
								{/if}

								{#if discoverData.missing_essentials && discoverData.missing_essentials.items.length > 0}
									<HomeSection section={discoverData.missing_essentials} />
								{/if}
							</div>
						</div>
					{/if}

					{#if hasFromYourLibrary}
						<div>
							<SectionDivider label="From Your Library">
								{#snippet icon()}<Library class="w-3.5 h-3.5" />{/snippet}
							</SectionDivider>

							<div class="discover-section-enter space-y-2">
								{#if discoverData.rediscover && discoverData.rediscover.items.length > 0}
									<HomeSection section={discoverData.rediscover} />
								{/if}

								{#if discoverData.lastfm_recent_scrobbles && discoverData.lastfm_recent_scrobbles.items.length > 0}
									<HomeSection section={discoverData.lastfm_recent_scrobbles} />
								{/if}
							</div>
						</div>
					{/if}

					{#if hasBrowseGenres}
						<div>
							<SectionDivider label="Browse Genres">
								{#snippet icon()}<LayoutGrid class="w-3.5 h-3.5" />{/snippet}
							</SectionDivider>

							<div class="discover-section-enter space-y-2">
								{#if discoverData.unexplored_genres && shuffledGenres.length > 0}
									<div class="mt-4 mb-4">
										<GenrePills
											title={discoverData.unexplored_genres.title}
											genres={shuffledGenres}
											onShuffle={shuffleGenres}
										/>
									</div>
								{/if}

								{#if discoverData.genre_list && discoverData.genre_list.items.length > 0}
									<div class="mt-4 mb-4">
										<GenreGrid
											title={discoverData.genre_list.title}
											genres={discoverData.genre_list.items}
											genreArtists={discoverData.genre_artists}
											genreArtistImages={discoverData.genre_artist_images}
										/>
									</div>
								{/if}
							</div>
						</div>
					{/if}

					{#if hasTrending}
						<div>
							<SectionDivider label="Trending Now">
								{#snippet icon()}<TrendingUp class="w-3.5 h-3.5" />{/snippet}
							</SectionDivider>

							<div class="discover-section-enter space-y-2">
								{#if discoverData.globally_trending && discoverData.globally_trending.items.length > 0}
									<HomeSection section={discoverData.globally_trending} />
								{/if}

								{#if discoverData.lastfm_weekly_artist_chart && discoverData.lastfm_weekly_artist_chart.items.length > 0}
									<HomeSection section={discoverData.lastfm_weekly_artist_chart} />
								{/if}

								{#if discoverData.lastfm_weekly_album_chart && discoverData.lastfm_weekly_album_chart.items.length > 0}
									<HomeSection section={discoverData.lastfm_weekly_album_chart} />
								{/if}
							</div>
						</div>
					{/if}

					{#if !hasContent && servicePrompts.length > 0}
						<div class="flex flex-col items-center justify-center py-12 sm:py-16">
							<Compass class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-base-content/50" />
							<h2 class="mb-2 text-center text-xl font-bold sm:text-2xl">
								Nothing to Discover Yet
							</h2>
							<p class="mb-6 max-w-md px-4 text-center text-sm text-base-content/70 sm:text-base">
								Connect a music service to get recommendations. The more you connect, the better
								they get.
							</p>
							<a href="/settings" class="btn btn-primary">Connect Services</a>
						</div>
					{:else if !hasContent}
						<div class="flex flex-col items-center justify-center py-12 sm:py-16">
							<Compass class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-base-content/50" />
							<h2 class="mb-2 text-center text-xl font-bold sm:text-2xl">Still Loading</h2>
							<p class="mb-6 max-w-md px-4 text-center text-sm text-base-content/70 sm:text-base">
								Your recommendations are still loading. Try refreshing.
							</p>
							<button
								class="btn btn-primary"
								onclick={() => void handleRefresh()}
								disabled={refreshing}
							>
								{#if refreshing}
									<span class="loading loading-spinner loading-sm"></span>
								{/if}
								Refresh Recommendations
							</button>
						</div>
					{/if}
				</div>
			{/if}
		</div>
	{/if}
</div>

<DiscoverQueueModal bind:open={queueModalOpen} source={activeSource} />
<PlaylistDiscoveryModal bind:open={playlistDiscoverOpen} source={activeSource} />
