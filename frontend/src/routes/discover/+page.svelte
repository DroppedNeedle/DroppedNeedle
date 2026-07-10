<script lang="ts">
	import HomeSection from '$lib/components/HomeSection.svelte';
	import DailyMixCard from '$lib/components/DailyMixCard.svelte';
	import RadioCard from '$lib/components/RadioCard.svelte';
	import TopPicksDeck from '$lib/components/discover/TopPicksDeck.svelte';
	import PlayableSection from '$lib/components/discover/PlayableSection.svelte';
	import ListeningLounge from '$lib/components/discover/ListeningLounge.svelte';
	import DiscoverZoneNav from '$lib/components/discover/DiscoverZoneNav.svelte';
	import GenrePills from '$lib/components/GenrePills.svelte';
	import GenreGrid from '$lib/components/GenreGrid.svelte';
	import DiscoverQueueDeck from '$lib/components/discover/DiscoverQueueDeck.svelte';
	import TasteGraphZone from '$lib/components/discover/TasteGraphZone.svelte';
	import PlaylistDiscoveryModal from '$lib/components/PlaylistDiscoveryModal.svelte';
	import WeeklyExploration from '$lib/components/WeeklyExploration.svelte';
	import ServicePromptCard from '$lib/components/ServicePromptCard.svelte';
	import DiscoverArtistHero from '$lib/components/DiscoverArtistHero.svelte';
	import DiscoverArtistMiniBand from '$lib/components/DiscoverArtistMiniBand.svelte';
	import SectionDivider from '$lib/components/SectionDivider.svelte';
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import PageHero from '$lib/ui/PageHero.svelte';
	import { api } from '$lib/api/client';
	import { isDismissed } from '$lib/utils/dismissedPrompts';
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
		Heart,
		SlidersHorizontal
	} from 'lucide-svelte';
	import { getDiscoverQuery } from '$lib/queries/discover/DiscoverQuery.svelte';
	import { getSectionPrefsQuery } from '$lib/queries/section-prefs/SectionPrefsQuery.svelte';
	import { discoverHasContent } from '$lib/utils/discoverContent';
	import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { API } from '$lib/constants';

	let playlistDiscoverOpen = $state(false);

	const discoverQuery = getDiscoverQuery();
	const sectionPrefsQuery = getSectionPrefsQuery();
	// client-only chrome: the backend can't blank an action card, so filter here
	const playlistDiscoveryEnabled = $derived(
		sectionPrefsQuery.data?.pages?.discover?.find((s) => s.key === 'playlist_discovery')?.enabled ??
			true
	);
	const discoverData = $derived(discoverQuery.data ?? null);
	const loading = $derived(discoverQuery.isLoading);
	const refreshing = $derived(discoverQuery.isFetching && !discoverQuery.isLoading);
	const isUpdating = $derived(discoverQuery.isRefetching && !!discoverData);
	const lastUpdated = $derived(
		discoverQuery.dataUpdatedAt ? new Date(discoverQuery.dataUpdatedAt) : null
	);
	const error = $derived(discoverQuery.error?.message ?? '');

	async function handleRefresh() {
		await api.global.post(API.discoverRefresh());
		await invalidateQueriesWithPersister({
			queryKey: DiscoverQueryKeyFactory.discover(authStore.user?.id)
		});
	}

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
		!!discoverData?.weekly_exploration && discoverData.weekly_exploration.tracks.length > 0
	);

	let queueEnabled = $derived(discoverData?.discover_queue_enabled ?? true);

	let hasMadeForYou = $derived(
		(discoverData?.daily_mixes?.length ?? 0) > 0 || (discoverData?.radio_sections?.length ?? 0) > 0
	);

	let hasLounge = $derived(
		(discoverData?.listeners_like_you?.items?.length ?? 0) > 0 ||
			(discoverData?.top_picks?.items?.length ?? 0) > 3
	);

	let zones = $derived.by(() => {
		const list: { id: string; label: string }[] = [{ id: 'zone-taste', label: 'Taste Graph' }];
		if (queueEnabled) list.push({ id: 'zone-queue', label: 'Queue' });
		if (hasBecauseYouListened) list.push({ id: 'zone-because', label: 'For You' });
		if (hasMadeForYou) list.push({ id: 'zone-made', label: 'Made For You' });
		if (hasNewFresh) list.push({ id: 'zone-fresh', label: 'New' });
		if (hasFromYourLibrary) list.push({ id: 'zone-library', label: 'Your Library' });
		if (hasBrowseGenres) list.push({ id: 'zone-genres', label: 'Genres' });
		if ((discoverData?.top_picks?.items?.length ?? 0) > 0)
			list.push({ id: 'zone-picks', label: 'Picks' });
		if (hasLounge) list.push({ id: 'zone-lounge', label: 'Lounge' });
		if (hasTrending) list.push({ id: 'zone-trending', label: 'Charts' });
		return list;
	});

	let hasBecauseYouListened = $derived(
		(discoverData?.because_you_listen_to?.length ?? 0) > 0 ||
			(discoverData?.artists_you_might_like?.items?.length ?? 0) > 0 ||
			(discoverData?.popular_in_your_genres?.items?.length ?? 0) > 0
	);

	let hasNewFresh = $derived(
		(discoverData?.fresh_releases?.items?.length ?? 0) > 0 ||
			(discoverData?.new_from_followed?.items?.length ?? 0) > 0 ||
			(discoverData?.missing_essentials?.items?.length ?? 0) > 0
	);

	let hasFromYourLibrary = $derived(
		(discoverData?.rediscover?.items?.length ?? 0) > 0 ||
			(discoverData?.anniversaries?.items?.length ?? 0) > 0 ||
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
	<PageHero
		title="Discover"
		subtitle="Music recommendations based on what you listen to."
		eyebrow="Your discovery engine"
		tint="rgb(var(--brand-discover))"
		{loading}
		isUpdating={isUpdating || isUpdatingInBackground}
		{lastUpdated}
	>
		{#snippet icon()}
			<Compass class="h-7 w-7" />
		{/snippet}
	</PageHero>

	{#if error && !discoverData}
		<div class="px-4 sm:px-6 lg:px-8">
			<div
				class="mt-10 flex flex-col items-center gap-4 rounded-2xl border border-base-content/8 bg-base-200/50 px-6 py-12 text-center"
			>
				<CircleAlert class="h-10 w-10 text-base-content/40" />
				<p class="text-sm text-base-content/70">{error}</p>
				<button class="btn btn-primary" onclick={() => discoverQuery.refetch()}>Try Again</button>
			</div>
		</div>
	{:else}
		<div class="px-4 pb-12 sm:px-6 lg:px-8">
			{#if (loading && !discoverData) || isBuilding}
				{#if isBuilding}
					<div
						class="mb-8 flex items-center justify-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 px-5 py-4"
					>
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
				<DiscoverZoneNav {zones}>
					{#snippet action()}
						<a
							href="/settings?tab=discover"
							class="btn btn-ghost btn-xs gap-1.5 rounded-full bg-base-content/6 text-base-content/60 hover:text-primary"
							title="Choose which sections appear here"
						>
							<SlidersHorizontal class="h-3.5 w-3.5" />
							<span class="hidden sm:inline">Customise</span>
						</a>
					{/snippet}
				</DiscoverZoneNav>

				<div class="space-y-10 sm:space-y-12" class:is-refreshing={isUpdatingInBackground}>
					<div id="zone-taste" class="discover-section-enter scroll-mt-14">
						<TasteGraphZone />
					</div>

					{#if queueEnabled}
						<div id="zone-queue" class="discover-section-enter scroll-mt-14">
							<DiscoverQueueDeck />
						</div>
					{/if}

					<!-- connect-service nudges: kept, but music comes first -->
					{#if servicePrompts.length > 0 && hasContent}
						<details
							class="group rounded-2xl border border-base-content/8 bg-base-200/50 transition-colors open:bg-base-200/50 hover:border-primary/30"
						>
							<summary
								class="flex cursor-pointer list-none items-center gap-3 px-5 py-3.5 [&::-webkit-details-marker]:hidden"
							>
								<Sparkles class="h-4 w-4 shrink-0 text-accent" />
								<span class="flex-1 text-sm text-base-content/70 group-hover:text-base-content"
									>Make Discover smarter — {servicePrompts.length} service{servicePrompts.length ===
									1
										? ''
										: 's'} to connect</span
								>
								<span
									class="font-mono text-[0.62rem] font-bold uppercase tracking-[0.2em] text-base-content/40 group-open:hidden"
									>Show</span
								>
								<span
									class="hidden font-mono text-[0.62rem] font-bold uppercase tracking-[0.2em] text-base-content/40 group-open:inline"
									>Hide</span
								>
							</summary>
							<div class="space-y-3 px-4 pb-4">
								{#each servicePrompts as prompt, i (`service-prompt-${prompt.service}-${i}`)}
									<ServicePromptCard {prompt} ondismiss={handlePromptDismiss} />
								{/each}
							</div>
						</details>
					{:else if servicePrompts.length > 0}
						<div class="space-y-3">
							{#each servicePrompts as prompt, i (`service-prompt-${prompt.service}-${i}`)}
								<ServicePromptCard {prompt} ondismiss={handlePromptDismiss} />
							{/each}
						</div>
					{/if}

					{#if hasWeeklyExploration && discoverData.weekly_exploration}
						<div class="discover-section-enter">
							<WeeklyExploration section={discoverData.weekly_exploration} />
						</div>
					{/if}

					{#if playlistDiscoveryEnabled}
						<div class="discover-section-enter">
							<button
								type="button"
								class="group flex w-full cursor-pointer items-center gap-4 rounded-2xl border border-base-content/8 bg-base-200/50 px-5 py-5 text-left transition-colors hover:border-primary/30 hover:bg-base-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100"
								onclick={() => (playlistDiscoverOpen = true)}
							>
								<div
									class="flex h-11 w-11 shrink-0 items-center justify-center rounded-xl bg-base-content/6"
								>
									<Wand2 class="h-5 w-5 text-accent" />
								</div>
								<div class="min-w-0 flex-1">
									<h3 class="font-display text-sm font-semibold tracking-tight sm:text-base">
										Discover for a Playlist
									</h3>
									<p class="mt-0.5 text-xs text-base-content/50">
										Get album suggestions based on any playlist.
									</p>
								</div>
								<Sparkles
									class="h-4 w-4 shrink-0 text-base-content/40 transition-transform duration-300 group-hover:translate-x-1 group-hover:text-primary"
								/>
							</button>
						</div>
					{/if}

					<div id="zone-because" class="scroll-mt-14">
						<SectionDivider label="Because You Listened">
							{#snippet icon()}<Heart class="h-3.5 w-3.5" />{/snippet}
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

					{#if hasMadeForYou}
						<div id="zone-made" class="scroll-mt-14">
							<SectionDivider label="Made For You">
								{#snippet icon()}<Sparkles class="h-3.5 w-3.5" />{/snippet}
							</SectionDivider>

							<div class="discover-section-enter space-y-8 sm:space-y-10">
								{#if discoverData.daily_mixes?.length}
									<section aria-label="Daily mixes">
										<h3
											class="mb-4 flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
										>
											<Music2 class="h-4 w-4 text-accent" />
											Daily Mixes
										</h3>
										<div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
											{#each discoverData.daily_mixes as mix, i (`${mix.title}-${i}`)}
												<DailyMixCard section={mix} />
											{/each}
										</div>
									</section>
								{/if}

								{#if discoverData.radio_sections?.length}
									<section aria-label="Radio stations">
										<h3
											class="mb-4 flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
										>
											<Radio class="h-4 w-4 text-accent" />
											Radio Stations
										</h3>
										<div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
											{#each discoverData.radio_sections as radio (`radio-${radio.radio_seed_id}`)}
												<RadioCard
													seedType={radio.radio_seed_type ?? 'artist'}
													seedId={radio.radio_seed_id ?? ''}
													initialSection={radio}
												/>
											{/each}
										</div>
									</section>
								{/if}
							</div>
						</div>
					{/if}

					{#if hasNewFresh}
						<div id="zone-fresh" class="scroll-mt-14">
							<SectionDivider label="New & Fresh">
								{#snippet icon()}<Music class="h-3.5 w-3.5" />{/snippet}
							</SectionDivider>

							<div class="discover-section-enter space-y-2">
								{#if discoverData.fresh_releases && discoverData.fresh_releases.items.length > 0}
									<PlayableSection
										section={discoverData.fresh_releases}
										sectionKey="fresh_releases"
									/>
								{/if}

								{#if discoverData.new_from_followed && discoverData.new_from_followed.items.length > 0}
									<PlayableSection
										section={discoverData.new_from_followed}
										headerLink="/following/new-releases"
										sectionKey="new_from_followed"
									/>
								{/if}

								{#if discoverData.missing_essentials && discoverData.missing_essentials.items.length > 0}
									<PlayableSection
										section={discoverData.missing_essentials}
										sectionKey="missing_essentials"
									/>
								{/if}
							</div>
						</div>
					{/if}

					{#if hasFromYourLibrary}
						<div id="zone-library" class="scroll-mt-14">
							<SectionDivider label="From Your Library">
								{#snippet icon()}<Library class="h-3.5 w-3.5" />{/snippet}
							</SectionDivider>

							<div class="discover-section-enter space-y-2">
								{#if discoverData.rediscover && discoverData.rediscover.items.length > 0}
									<HomeSection section={discoverData.rediscover} />
								{/if}

								{#if discoverData.anniversaries && discoverData.anniversaries.items.length > 0}
									<HomeSection section={discoverData.anniversaries} />
								{/if}

								{#if discoverData.lastfm_recent_scrobbles && discoverData.lastfm_recent_scrobbles.items.length > 0}
									<HomeSection section={discoverData.lastfm_recent_scrobbles} />
								{/if}
							</div>
						</div>
					{/if}

					{#if hasBrowseGenres}
						<div id="zone-genres" class="scroll-mt-14">
							<SectionDivider label="Browse Genres">
								{#snippet icon()}<LayoutGrid class="h-3.5 w-3.5" />{/snippet}
							</SectionDivider>

							<div class="discover-section-enter space-y-6">
								{#if discoverData.unexplored_genres && shuffledGenres.length > 0}
									<GenrePills
										title={discoverData.unexplored_genres.title}
										genres={shuffledGenres}
										onShuffle={shuffleGenres}
									/>
								{/if}

								{#if discoverData.genre_list && discoverData.genre_list.items.length > 0}
									<GenreGrid
										title={discoverData.genre_list.title}
										genres={discoverData.genre_list.items}
										genreArtistImages={discoverData.genre_artist_images}
									/>
								{/if}
							</div>
						</div>
					{/if}

					{#if (discoverData.top_picks && discoverData.top_picks.items.length > 0) || hasLounge || hasTrending}
						<div class="flex items-center gap-3">
							<span
								class="font-mono text-[0.62rem] font-bold uppercase tracking-[0.2em] text-base-content/35"
								>Wider world — charts &amp; other listeners, not your taste profile</span
							>
							<div class="h-px flex-1 bg-base-content/10"></div>
							<a
								href="/settings?tab=discover"
								class="btn btn-ghost btn-xs rounded-full text-base-content/50 hover:text-primary"
								>Hide these</a
							>
						</div>
					{/if}

					{#if discoverData.top_picks && discoverData.top_picks.items.length > 0}
						<div id="zone-picks" class="discover-section-enter scroll-mt-14">
							<TopPicksDeck section={discoverData.top_picks} />
						</div>
					{/if}

					{#if hasLounge}
						<div id="zone-lounge" class="discover-section-enter scroll-mt-14">
							<ListeningLounge
								section={discoverData.listeners_like_you}
								topPicks={discoverData.top_picks}
							/>
						</div>
					{/if}

					{#if hasTrending}
						<div id="zone-trending" class="scroll-mt-14">
							<SectionDivider label="Trending Now">
								{#snippet icon()}<TrendingUp class="h-3.5 w-3.5" />{/snippet}
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
						<div
							class="flex flex-col items-center rounded-2xl border border-dashed border-base-content/12 px-6 py-12 text-center sm:py-16"
						>
							<Compass class="mb-4 h-10 w-10 text-base-content/40 sm:mb-5 sm:h-12 sm:w-12" />
							<h2 class="font-display text-xl font-bold tracking-tight sm:text-2xl">
								Nothing to Discover Yet
							</h2>
							<p class="mt-2 max-w-md text-sm text-base-content/60 sm:text-base">
								Connect a music service to get recommendations. The more you connect, the better
								they get.
							</p>
							<a href="/settings" class="btn btn-primary mt-6">Connect Services</a>
						</div>
					{:else if !hasContent && !queueEnabled}
						<div
							class="flex flex-col items-center rounded-2xl border border-dashed border-base-content/12 px-6 py-12 text-center sm:py-16"
						>
							<SlidersHorizontal
								class="mb-4 h-10 w-10 text-base-content/40 sm:mb-5 sm:h-12 sm:w-12"
							/>
							<h2 class="font-display text-xl font-bold tracking-tight sm:text-2xl">
								You've Hidden Every Section
							</h2>
							<p class="mt-2 max-w-md text-sm text-base-content/60 sm:text-base">
								Turn some discovery sections back on to fill this page.
							</p>
							<a href="/settings?tab=discover" class="btn btn-primary mt-6 gap-2">
								<SlidersHorizontal class="h-4 w-4" />
								Customise Sections
							</a>
						</div>
					{:else if !hasContent}
						<div
							class="flex flex-col items-center rounded-2xl border border-dashed border-base-content/12 px-6 py-12 text-center sm:py-16"
						>
							<Compass class="mb-4 h-10 w-10 text-base-content/40 sm:mb-5 sm:h-12 sm:w-12" />
							<h2 class="font-display text-xl font-bold tracking-tight sm:text-2xl">
								Still Loading
							</h2>
							<p class="mt-2 max-w-md text-sm text-base-content/60 sm:text-base">
								Your recommendations are still loading. Try refreshing.
							</p>
							<button
								class="btn btn-primary mt-6"
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

<PlaylistDiscoveryModal bind:open={playlistDiscoverOpen} />
