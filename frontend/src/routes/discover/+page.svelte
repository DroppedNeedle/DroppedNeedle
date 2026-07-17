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
	import PlaylistDiscoveryModal from '$lib/components/PlaylistDiscoveryModal.svelte';
	import WeeklyExploration from '$lib/components/WeeklyExploration.svelte';
	import ServicePromptCard from '$lib/components/ServicePromptCard.svelte';
	import DiscoverArtistHero from '$lib/components/DiscoverArtistHero.svelte';
	import DiscoverArtistMiniBand from '$lib/components/DiscoverArtistMiniBand.svelte';
	import SectionDivider from '$lib/components/SectionDivider.svelte';
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import LiveUpdatingBadge from '$lib/components/LiveUpdatingBadge.svelte';
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
	import { getIgnoreDiscoveryMutation } from '$lib/queries/discover/DiscoverMutations.svelte';
	import type { TopPickItem } from '$lib/types';
	import { getSectionPrefsQuery } from '$lib/queries/section-prefs/SectionPrefsQuery.svelte';
	import { discoverHasContent } from '$lib/utils/discoverContent';
	import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { API } from '$lib/constants';

	let playlistDiscoverOpen = $state(false);

	const discoverQuery = getDiscoverQuery();
	const ignoreDiscoveryMutation = getIgnoreDiscoveryMutation();
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
		discoverData?.generated_at ? new Date(discoverData.generated_at * 1000) : null
	);
	const error = $derived(discoverQuery.error?.message ?? '');

	async function handleRefresh() {
		await api.global.post(API.discoverRefresh());
		await invalidateQueriesWithPersister({
			queryKey: DiscoverQueryKeyFactory.discover(authStore.user?.id)
		});
	}

	async function handleIgnoreTopPick(pick: TopPickItem) {
		if (!pick.album.mbid || !pick.album.artist_mbid) return;
		await ignoreDiscoveryMutation.mutateAsync({
			releaseGroupMbid: pick.album.mbid,
			artistMbid: pick.album.artist_mbid,
			releaseName: pick.album.name,
			artistName: pick.album.artist_name ?? ''
		});
	}

	let hasContent = $derived(discoverHasContent(discoverData));
	// build-time degradation summary; explains an empty or slow page instead of
	// leaving the user on endless skeletons
	const degradedSources = $derived(Object.keys(discoverData?.service_status ?? {}));
	const degradedLabel = $derived(
		degradedSources.map((s) => s.charAt(0).toUpperCase() + s.slice(1)).join(', ')
	);
	const degradedVerb = $derived(degradedSources.length > 1 ? 'are' : 'is');
	let isBuilding = $derived(
		!!discoverData && !hasContent && (!!discoverData.refreshing || isUpdating)
	);
	let isUpdatingInBackground = $derived(!!discoverData?.refreshing && hasContent);
	function sectionUpdating(section: string): boolean {
		return discoverData?.section_status?.[section] === 'updating';
	}
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
		const list: { id: string; label: string }[] = [];
		if (queueEnabled) list.push({ id: 'zone-queue', label: 'Queue' });
		if ((discoverData?.top_picks?.items?.length ?? 0) > 0)
			list.push({ id: 'zone-picks', label: 'Top Picks' });
		if (hasLounge) list.push({ id: 'zone-lounge', label: 'Lounge' });
		if (hasWeeklyExploration) list.push({ id: 'zone-weekly', label: 'Weekly' });
		if (playlistDiscoveryEnabled) list.push({ id: 'zone-playlist', label: 'Playlist' });
		if (hasMadeForYou) list.push({ id: 'zone-made', label: 'Made For You' });
		list.push({ id: 'zone-because', label: 'For You' });
		if (hasNewFresh) list.push({ id: 'zone-fresh', label: 'New' });
		if (hasFromYourLibrary) list.push({ id: 'zone-library', label: 'Your Library' });
		if (hasBrowseGenres) list.push({ id: 'zone-genres', label: 'Genres' });
		if (hasTrending) list.push({ id: 'zone-trending', label: 'Trending' });
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
	<PageHeader
		subtitle="Music recommendations based on what you listen to."
		gradientClass="bg-gradient-to-br from-info/30 via-primary/20 to-secondary/10"
		{loading}
		isUpdating={isUpdating || isUpdatingInBackground}
		refreshing={refreshing || isUpdatingInBackground}
		{lastUpdated}
		onRefresh={() => void handleRefresh()}
		refreshLabel="Refresh recommendations"
	>
		{#snippet title()}
			<Compass class="inline h-8 w-8 sm:h-10 sm:w-10 lg:h-12 lg:w-12 mr-2 align-text-bottom" />
			Discover
		{/snippet}
	</PageHeader>

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
					<div class="mb-8 flex flex-col items-center justify-center gap-2 px-4 text-center">
						<div class="flex items-center justify-center gap-3">
							<LiveUpdatingBadge label="Preparing your Discover page" />
							<p class="text-sm text-base-content/60">
								Your recommendations will appear here as each section becomes ready.
							</p>
						</div>
						{#if degradedSources.length > 0}
							<p class="text-xs text-base-content/50">
								{degradedLabel}
								{degradedVerb} temporarily unavailable, so this may take longer than usual.
							</p>
						{/if}
					</div>
				{/if}
				<div class="space-y-8" aria-label="Loading Discover sections">
					{#each ['Top Picks', 'Made For You', 'Because You Listened', 'New & Fresh'] as label (label)}
						<section>
							<SectionDivider {label} updating />
							<CarouselSkeleton />
						</section>
					{/each}
				</div>
			{:else if discoverData}
				<!-- a render error in any section degrades to the failed card below instead
				     of crashing the whole SPA (the #147 freeze) -->
				<svelte:boundary>
					<DiscoverZoneNav {zones}>
						{#snippet action()}
							<a
								href="/settings?tab=discover"
								class="btn btn-ghost btn-xs gap-1.5 text-base-content/60 hover:text-primary"
								title="Choose which sections appear here"
							>
								<SlidersHorizontal class="h-3.5 w-3.5" />
								<span class="hidden sm:inline">Customise</span>
							</a>
						{/snippet}
					</DiscoverZoneNav>
					<div class="space-y-10 sm:space-y-12" class:is-refreshing={isUpdatingInBackground}>
						{#if queueEnabled}
							<div id="zone-queue" class="scroll-mt-28">
								<DiscoverQueueDeck />
							</div>
						{/if}

						{#if discoverData.top_picks && discoverData.top_picks.items.length > 0}
							<div id="zone-picks" class="scroll-mt-28">
								<TopPicksDeck
									section={discoverData.top_picks}
									updating={sectionUpdating('picks')}
									onignore={handleIgnoreTopPick}
								/>
							</div>
						{/if}

						{#if hasLounge}
							<div id="zone-lounge" class="scroll-mt-28">
								<ListeningLounge
									section={discoverData.listeners_like_you}
									topPicks={discoverData.top_picks}
									updating={sectionUpdating('lounge')}
								/>
							</div>
						{/if}

						{#if hasWeeklyExploration && discoverData.weekly_exploration}
							<div id="zone-weekly" class="scroll-mt-28">
								<WeeklyExploration
									section={discoverData.weekly_exploration}
									updating={sectionUpdating('weekly')}
								/>
							</div>
						{/if}

						<div id="zone-playlist" class="scroll-mt-28">
							<div class="grid grid-cols-1 gap-4">
								{#if playlistDiscoveryEnabled}
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
								{/if}
							</div>
						</div>

						{#if hasMadeForYou}
							<div id="zone-made" class="scroll-mt-28">
								<SectionDivider label="Made For You" updating={sectionUpdating('made')}>
									{#snippet icon()}<Sparkles class="w-3.5 h-3.5" />{/snippet}
								</SectionDivider>

								<div class="space-y-8 sm:space-y-10">
									{#if discoverData.daily_mixes?.length}
										<div>
											<h3
												class="section-title text-sm font-semibold text-base-content/70 mb-3 flex items-center gap-2"
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
												class="section-title text-sm font-semibold text-base-content/70 mb-3 flex items-center gap-2"
											>
												<Radio class="h-4 w-4" />
												Radio Stations
											</h3>
											<div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
												{#each discoverData.radio_sections as radio, i (`radio-${radio.radio_seed_id}-${i}`)}
													<RadioCard
														seedType={radio.radio_seed_type ?? 'artist'}
														seedId={radio.radio_seed_id ?? ''}
														initialSection={radio}
													/>
												{/each}
											</div>
										</div>
									{/if}
								</div>
							</div>
						{/if}

						<div id="zone-because" class="scroll-mt-28">
							<SectionDivider label="Because You Listened" updating={sectionUpdating('because')}>
								{#snippet icon()}<Heart class="w-3.5 h-3.5" />{/snippet}
							</SectionDivider>

							{#if hasBecauseYouListened}
								<div class="space-y-5 sm:space-y-6">
									{#each discoverData.because_you_listen_to as entry, i (`${entry.seed_artist_mbid || entry.seed_artist}-${i}`)}
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
							<div id="zone-fresh" class="scroll-mt-28">
								<SectionDivider label="New & Fresh" updating={sectionUpdating('fresh')}>
									{#snippet icon()}<Music class="w-3.5 h-3.5" />{/snippet}
								</SectionDivider>

								<div class="space-y-2">
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
							<div id="zone-library" class="scroll-mt-28">
								<SectionDivider label="From Your Library" updating={sectionUpdating('library')}>
									{#snippet icon()}<Library class="w-3.5 h-3.5" />{/snippet}
								</SectionDivider>

								<div class="space-y-2">
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
							<div id="zone-genres" class="scroll-mt-28">
								<SectionDivider label="Browse Genres" updating={sectionUpdating('genres')}>
									{#snippet icon()}<LayoutGrid class="w-3.5 h-3.5" />{/snippet}
								</SectionDivider>

								<div class="space-y-2">
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
												genreArtwork={discoverData.genre_artwork}
											/>
										</div>
									{/if}
								</div>
							</div>
						{/if}

						{#if hasTrending}
							<div id="zone-trending" class="scroll-mt-28">
								<SectionDivider label="Trending Now" updating={sectionUpdating('trending')}>
									{#snippet icon()}<TrendingUp class="w-3.5 h-3.5" />{/snippet}
								</SectionDivider>

								<div class="space-y-2">
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
						{:else if !hasContent && degradedSources.length > 0}
							<div class="flex flex-col items-center justify-center py-12 sm:py-16">
								<CircleAlert class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-base-content/50" />
								<h2 class="mb-2 text-center text-xl font-bold sm:text-2xl">
									Recommendations Unavailable
								</h2>
								<p class="mb-6 max-w-md px-4 text-center text-sm text-base-content/70 sm:text-base">
									{degradedLabel}
									{degradedVerb} temporarily unavailable, so we couldn't build recommendations. We'll
									keep retrying in the background - your page will fill in as soon as it recovers.
								</p>
								<button
									class="btn btn-primary"
									onclick={() => void handleRefresh()}
									disabled={refreshing}
								>
									{#if refreshing}
										<span class="loading loading-spinner loading-sm"></span>
									{/if}
									Retry Now
								</button>
							</div>
						{:else if !hasContent && !queueEnabled}
							<div class="flex flex-col items-center justify-center py-12 sm:py-16">
								<SlidersHorizontal
									class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-base-content/50"
								/>
								<h2 class="mb-2 text-center text-xl font-bold sm:text-2xl">
									You've hidden every section
								</h2>
								<p class="mb-6 max-w-md px-4 text-center text-sm text-base-content/70 sm:text-base">
									Turn some discovery sections back on to fill this page.
								</p>
								<a href="/settings?tab=discover" class="btn btn-primary gap-2">
									<SlidersHorizontal class="h-4 w-4" />
									Customise Sections
								</a>
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

					{#snippet failed(_error, reset)}
						<div class="flex flex-col items-center justify-center py-12 sm:py-16">
							<CircleAlert class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-base-content/50" />
							<h2 class="mb-2 text-center text-xl font-bold sm:text-2xl">Something Went Wrong</h2>
							<p class="mb-6 max-w-md px-4 text-center text-sm text-base-content/70 sm:text-base">
								Part of the Discover page failed to render. The rest of the app still works.
							</p>
							<button class="btn btn-primary" onclick={() => reset()}>Try Again</button>
						</div>
					{/snippet}
				</svelte:boundary>
			{/if}
		</div>
	{/if}
</div>

<PlaylistDiscoveryModal bind:open={playlistDiscoverOpen} />
