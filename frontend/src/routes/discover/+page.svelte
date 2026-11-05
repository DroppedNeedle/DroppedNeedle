<script lang="ts">
	import '$lib/features/discover/components/discover.css';
	import HomeSection from '$lib/features/discover/components/HomeSection.svelte';
	import ServiceErrorState from '$lib/ui/ServiceErrorState.svelte';
	import DailyMixCard from '$lib/features/discover/components/DailyMixCard.svelte';
	import RadioCard from '$lib/features/discover/components/RadioCard.svelte';
	import TopPicksDeck from '$lib/features/discover/components/TopPicksDeck.svelte';
	import PlayableSection from '$lib/features/discover/components/PlayableSection.svelte';
	import ListeningLounge from '$lib/features/discover/components/ListeningLounge.svelte';
	import JumpTabs from '$lib/ui/JumpTabs.svelte';
	import DiscoverHeader from '$lib/features/discover/components/DiscoverHeader.svelte';
	import DiscoverPlaylistBand from '$lib/features/discover/components/DiscoverPlaylistBand.svelte';
	import DiscoverBecauseEmpty from '$lib/features/discover/components/DiscoverBecauseEmpty.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import SourceBadge from '$lib/features/discover/components/SourceBadge.svelte';
	import GenrePills from '$lib/features/genre/components/GenrePills.svelte';
	import GenreGrid from '$lib/features/genre/components/GenreGrid.svelte';
	import DiscoverQueueDeck from '$lib/features/discover/components/DiscoverQueueDeck.svelte';
	import HiddenAlbumsModal from '$lib/features/discover/components/HiddenAlbumsModal.svelte';
	import PlaylistDiscoveryModal from '$lib/features/discover/components/PlaylistDiscoveryModal.svelte';
	import WeeklyExploration from '$lib/features/discover/components/WeeklyExploration.svelte';
	import ServicePromptCard from '$lib/features/discover/components/ServicePromptCard.svelte';
	import DiscoverArtistHero from '$lib/features/discover/components/DiscoverArtistHero.svelte';
	import DiscoverArtistMiniBand from '$lib/features/discover/components/DiscoverArtistMiniBand.svelte';
	import SectionDivider from '$lib/ui/SectionDivider.svelte';
	import CarouselSkeleton from '$lib/ui/CarouselSkeleton.svelte';
	import LiveUpdatingBadge from '$lib/ui/LiveUpdatingBadge.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { isDismissed } from '$lib/features/discover/state/dismissedPrompts';
	import { withBasePath } from '$lib/utils/basePath';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		EyeOff,
		Compass,
		CircleAlert,
		Sparkles,
		Music,
		Library,
		TrendingUp,
		LayoutGrid,
		Heart,
		Globe,
		SlidersHorizontal
	} from 'lucide-svelte';
	import { getDiscoverHomeV3Query } from '$lib/features/discover/queries/DiscoverV3Queries.svelte';
	import {
		createIgnoreDiscoveryV3Mutation,
		createRefreshDiscoverV3Mutation
	} from '$lib/features/discover/queries/DiscoverV3Mutations.svelte';
	import { useDiscoverActivity } from '$lib/features/discover/queries/DiscoverDemand.svelte';
	import type {
		ChartSection,
		SectionKey,
		SectionZone,
		TopPickItem
	} from '$lib/features/discover/queries/types';
	import type { KitIcon } from '$lib/ui/Button.svelte';
	import { getSectionPrefsQuery } from '$lib/features/settings/queries/section-prefs.svelte';
	import {
		becauseOf,
		picksOf,
		shelfOf,
		shelvesOf,
		weeklyOf,
		zoneSections
	} from '$lib/features/discover/queries/sections';
	import { albumRows, genreRows } from '$lib/features/discover/components/sectionRows';

	useDiscoverActivity(() => ({ feature: 'discover' }));

	let playlistDiscoverOpen = $state(false);
	let hiddenAlbumsOpen = $state(false);

	const discoverQuery = getDiscoverHomeV3Query();
	const ignoreDiscoveryMutation = createIgnoreDiscoveryV3Mutation();
	const refreshDiscoverMutation = createRefreshDiscoverV3Mutation();
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

	async function handleRefresh() {
		await refreshDiscoverMutation.mutateAsync();
	}

	async function handleIgnoreTopPick(pick: TopPickItem) {
		if (!pick.album.musicbrainz_id || !pick.album.artist_id) return;
		await ignoreDiscoveryMutation.mutateAsync({
			releaseGroupMbid: pick.album.musicbrainz_id,
			artistMbid: pick.album.artist_id,
			releaseName: pick.album.title,
			artistName: pick.album.artist_name ?? ''
		});
	}

	const sections = $derived(discoverData?.sections);
	const topPicks = $derived(picksOf(sections));
	const lounge = $derived(shelfOf(sections, 'listeners_like_you'));
	const weekly = $derived(weeklyOf(sections));
	const dailyMixes = $derived(shelvesOf(sections, 'daily_mixes'));
	const radioSections = $derived(shelvesOf(sections, 'radio_sections'));
	const because = $derived(becauseOf(sections));
	const artistsYouMightLike = $derived(shelfOf(sections, 'artists_you_might_like'));
	const popularInYourGenres = $derived(shelfOf(sections, 'popular_in_your_genres'));
	const missingEssentials = $derived(shelfOf(sections, 'missing_essentials'));
	const unexploredGenres = $derived(shelfOf(sections, 'unexplored_genres'));
	const genreList = $derived(shelfOf(sections, 'genre_list'));

	let hasContent = $derived((sections?.length ?? 0) > 0);
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
	const sectionsUpdating = $derived(!!discoverData?.refreshing);
	let dismissVersion = $state(0);
	let servicePrompts = $derived.by(() => {
		void dismissVersion;
		return (discoverData?.service_prompts ?? []).filter((p) => !isDismissed(p.service));
	});

	let queueEnabled = $derived(discoverData?.discover_queue_enabled ?? true);

	let hasLounge = $derived(!!lounge || (topPicks?.items.length ?? 0) > 3);

	let hasBecauseYouListened = $derived(zoneSections(sections, 'because_you_listened').length > 0);

	// Each section is a block with a width: a wide block takes a row of its
	// own, and two half blocks next to each other share one. Blocks follow the
	// order the server sends; the queue and the playlist band are the page's
	// own and keep their places among them.
	type Kicker = { label: string; icon: KitIcon };
	type Block = {
		id: string;
		width: 'wide' | 'half';
		/** The zone strip's link, for the first block of a zone. */
		nav?: { id: string; label: string };
		kicker?: Kicker;
	} & (
		| { kind: 'queue' | 'picks' | 'lounge' | 'weekly' | 'playlist' | 'because' }
		| { kind: 'mixes' | 'radio' | 'genres-explore' | 'genres-browse' }
		| { kind: 'shelf'; shelf: ChartSection; key: SectionKey; playable: boolean; link?: string }
		| { kind: 'trending-lead'; shelf: ChartSection }
	);

	const ZONES: Partial<Record<SectionZone, { id: string; nav: string } & Kicker>> = {
		made_for_you: { id: 'zone-made', nav: 'Made For You', label: 'Made For You', icon: Sparkles },
		because_you_listened: {
			id: 'zone-because',
			nav: 'For You',
			label: 'Because You Listened',
			icon: Heart
		},
		new_and_fresh: { id: 'zone-fresh', nav: 'New', label: 'New & Fresh', icon: Music },
		from_your_library: {
			id: 'zone-library',
			nav: 'Your Library',
			label: 'From Your Library',
			icon: Library
		},
		browse_genres: { id: 'zone-genres', nav: 'Genres', label: 'Browse Genres', icon: LayoutGrid },
		trending_now: { id: 'zone-trending', nav: 'Trending', label: 'Trending Now', icon: TrendingUp }
	};

	const SHELF_LINKS: Partial<Record<SectionKey, string>> = {
		new_from_followed: '/following/new-releases'
	};

	// zones that come before the playlist band and Because You Listened
	const EARLY_ZONES = new Set<SectionZone>(['essentials', 'for_you']);

	let blocks = $derived.by(() => {
		const list: Block[] = [];
		const seenZones: SectionZone[] = [];
		let playlistPlaced = false;
		let becausePlaced = false;
		const placePlaylist = () => {
			if (playlistPlaced) return;
			playlistPlaced = true;
			if (playlistDiscoveryEnabled)
				list.push({
					id: 'zone-playlist',
					kind: 'playlist',
					width: 'wide',
					nav: { id: 'zone-playlist', label: 'Playlist' }
				});
		};
		// For You always shows, with its empty state when nothing is there yet
		const placeBecause = () => {
			if (becausePlaced) return;
			becausePlaced = true;
			seenZones.push('because_you_listened');
			list.push({
				id: 'zone-because',
				kind: 'because',
				width: 'wide',
				nav: { id: 'zone-because', label: 'For You' }
			});
		};

		if (queueEnabled)
			list.push({
				id: 'zone-queue',
				kind: 'queue',
				width: 'wide',
				nav: { id: 'zone-queue', label: 'Queue' }
			});

		for (const section of sections ?? []) {
			const { key, zone, content } = section;
			if (!EARLY_ZONES.has(zone)) placePlaylist();
			if (!EARLY_ZONES.has(zone) && zone !== 'made_for_you') placeBecause();

			const zoneInfo = ZONES[zone];
			const id = `section-${key}`;
			const kicker = zoneInfo ? { label: zoneInfo.label, icon: zoneInfo.icon } : undefined;
			// the first block a zone places carries its strip link; a zone's
			// later wide blocks drop the repeated kicker
			const place = (block: Block) => {
				if (zoneInfo && !seenZones.includes(zone)) {
					seenZones.push(zone);
					block.id = zoneInfo.id;
					block.nav = { id: zoneInfo.id, label: zoneInfo.nav };
				} else if (block.width === 'wide') {
					block.kicker = undefined;
				}
				list.push(block);
			};

			if (key === 'top_picks') {
				if (topPicks)
					list.push({
						id: 'zone-picks',
						kind: 'picks',
						width: 'half',
						nav: { id: 'zone-picks', label: 'Top Picks' }
					});
				// the lounge also plays Top Picks' runners-up when it has no shelf
				if (!lounge && hasLounge)
					list.push({
						id: 'zone-lounge',
						kind: 'lounge',
						width: 'half',
						nav: { id: 'zone-lounge', label: 'Lounge' }
					});
			} else if (key === 'listeners_like_you') {
				if (hasLounge)
					list.push({
						id: 'zone-lounge',
						kind: 'lounge',
						width: 'half',
						nav: { id: 'zone-lounge', label: 'Lounge' }
					});
			} else if (key === 'weekly_exploration') {
				if (weekly)
					list.push({
						id: 'zone-weekly',
						kind: 'weekly',
						width: 'half',
						nav: { id: 'zone-weekly', label: 'Weekly' }
					});
			} else if (key === 'daily_mixes' || key === 'radio_sections') {
				if ((key === 'daily_mixes' ? dailyMixes : radioSections).length)
					place({
						id,
						kind: key === 'daily_mixes' ? 'mixes' : 'radio',
						width: 'wide',
						kicker
					});
			} else if (zone === 'because_you_listened') {
				// its shelves sit inside the Because You Listened block
				placeBecause();
			} else if (key === 'unexplored_genres') {
				if (unexploredGenres && shuffledGenres.length > 0)
					place({ id, kind: 'genres-explore', width: 'half', kicker });
			} else if (key === 'genre_list') {
				if (genreList) place({ id, kind: 'genres-browse', width: 'half', kicker });
			} else if (content.shape === 'shelf' && key === 'globally_trending') {
				place({ id, kind: 'trending-lead', width: 'wide', shelf: content.shelf });
			} else if (content.shape === 'shelf') {
				place({
					id,
					kind: 'shelf',
					width: 'half',
					kicker,
					shelf: content.shelf,
					key,
					playable: zone === 'new_and_fresh',
					link: SHELF_LINKS[key]
				});
			}
		}
		placePlaylist();
		placeBecause();
		return list;
	});

	// consecutive half blocks pair up; a half with no half neighbour runs wide
	let rows = $derived.by(() => {
		const out: Block[][] = [];
		for (let i = 0; i < blocks.length; i++) {
			const block = blocks[i];
			const next = blocks[i + 1];
			if (block.width === 'half' && next?.width === 'half') {
				out.push([block, next]);
				i++;
			} else {
				out.push([block]);
			}
		}
		return out;
	});

	let zones = $derived(blocks.flatMap((block) => (block.nav ? [block.nav] : [])));

	// covers the page already shows, for the playlist band's sleeves
	let bandCovers = $derived.by(() => {
		const ids = [
			...(topPicks?.items.map((pick) => pick.album.musicbrainz_id) ?? []),
			...albumRows(lounge).map((album) => album.musicbrainz_id),
			...albumRows(missingEssentials).map((album) => album.musicbrainz_id)
		].filter((id): id is string => !!id);
		return ids.filter((id, i) => ids.indexOf(id) === i).slice(0, 4);
	});

	let shuffledGenres = $state<
		{ name: string; listen_count?: number | null; artist_count?: number | null }[]
	>([]);
	$effect(() => {
		const items = unexploredGenres?.items;
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
	<DiscoverHeader
		{loading}
		isUpdating={isUpdating || isUpdatingInBackground}
		refreshing={refreshing || isUpdatingInBackground}
		{lastUpdated}
		onRefresh={() => void handleRefresh()}
	/>

	{#if discoverQuery.error && !discoverData}
		<ServiceErrorState
			error={discoverQuery.error}
			what="Discover"
			onretry={() => discoverQuery.refetch()}
		/>
	{:else}
		{#if servicePrompts.length > 0}
			<div class="dn-discover-col mb-6 grid gap-3 md:grid-cols-2 xl:grid-cols-3">
				{#each servicePrompts as prompt, i (`service-prompt-${prompt.service}-${i}`)}
					<ServicePromptCard {prompt} compact ondismiss={handlePromptDismiss} />
				{/each}
			</div>
		{/if}

		{#if (loading && !discoverData) || isBuilding}
			<div class="dn-discover-col">
				{#if isBuilding}
					<div class="mb-8 flex flex-col items-center justify-center gap-2 px-4 text-center">
						<div class="flex items-center justify-center gap-3">
							<LiveUpdatingBadge label="Preparing your Discover page" />
							<p class="text-sm text-muted">
								Your recommendations will appear here as each section becomes ready.
							</p>
						</div>
						{#if degradedSources.length > 0}
							<p class="text-xs text-muted">
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
			</div>
		{:else if discoverData}
			<!-- a render error in any section degrades to the failed card below instead
			     of crashing the whole SPA (the #147 freeze) -->
			<svelte:boundary>
				<JumpTabs sections={zones} look="pills" label="Jump to a discover section">
					{#snippet actions()}
						<Button
							variant="ghost"
							size="xs"
							icon={EyeOff}
							title="Hidden albums"
							onclick={() => (hiddenAlbumsOpen = true)}
						>
							<span class="hidden sm:inline">Hidden</span>
						</Button>
						<Button
							variant="ghost"
							size="xs"
							icon={SlidersHorizontal}
							href={withBasePath('/settings?tab=discover')}
							title="Choose which sections appear here"
						>
							<span class="hidden sm:inline">Customise</span>
						</Button>
					{/snippet}
				</JumpTabs>

				<div class="dn-discover-col dn-discover-rows" class:is-refreshing={isUpdatingInBackground}>
					{#each rows as row (row.map((block) => block.id).join('+'))}
						<div class="dn-discover-row" class:dn-discover-row--pair={row.length === 2}>
							{#each row as block, i (block.id)}
								<div
									id={block.id}
									class="dn-discover-block scroll-mt-32"
									class:dn-discover-block--ruled={i === 1 && block.kind === 'genres-browse'}
								>
									{@render blockBody(block, row.length === 2)}
								</div>
							{/each}
						</div>
					{/each}

					{#if !hasContent && servicePrompts.length > 0}
						<div class="flex flex-col items-center justify-center py-12 sm:py-16">
							<Compass class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-muted" />
							<h2 class="dn-panel__title mb-3 text-center">Nothing to Discover Yet</h2>
							<p class="mb-6 max-w-md px-4 text-center text-sm text-muted sm:text-base">
								Connect a music service to get recommendations. The more you connect, the better
								they get.
							</p>
							<Button
								variant="primary"
								href={withBasePath(authStore.isAdmin ? '/settings' : '/profile#scrobbling')}
								>Connect Services</Button
							>
						</div>
					{:else if !hasContent && degradedSources.length > 0}
						<div class="flex flex-col items-center justify-center py-12 sm:py-16">
							<CircleAlert class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-muted" />
							<h2 class="dn-panel__title mb-3 text-center">Recommendations Unavailable</h2>
							<p class="mb-6 max-w-md px-4 text-center text-sm text-muted sm:text-base">
								{degradedLabel}
								{degradedVerb} temporarily unavailable, so we couldn't build recommendations. We'll keep
								retrying in the background - your page will fill in as soon as it recovers.
							</p>
							<Button variant="primary" loading={refreshing} onclick={() => void handleRefresh()}>
								Retry Now
							</Button>
						</div>
					{:else if !hasContent && !queueEnabled}
						<div class="flex flex-col items-center justify-center py-12 sm:py-16">
							<SlidersHorizontal class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-muted" />
							<h2 class="dn-panel__title mb-3 text-center">You've hidden every section</h2>
							<p class="mb-6 max-w-md px-4 text-center text-sm text-muted sm:text-base">
								Turn some discovery sections back on to fill this page.
							</p>
							<Button
								variant="primary"
								icon={SlidersHorizontal}
								href={withBasePath('/settings?tab=discover')}
							>
								Customise Sections
							</Button>
						</div>
					{:else if !hasContent}
						<div class="flex flex-col items-center justify-center py-12 sm:py-16">
							<Compass class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-muted" />
							<h2 class="dn-panel__title mb-3 text-center">Still Loading</h2>
							<p class="mb-6 max-w-md px-4 text-center text-sm text-muted sm:text-base">
								Your recommendations are still loading. Try refreshing.
							</p>
							<Button variant="primary" loading={refreshing} onclick={() => void handleRefresh()}>
								Refresh Recommendations
							</Button>
						</div>
					{/if}
				</div>

				{#snippet failed(_error, reset)}
					<div class="flex flex-col items-center justify-center py-12 sm:py-16">
						<CircleAlert class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-muted" />
						<h2 class="dn-panel__title mb-3 text-center">Something Went Wrong</h2>
						<p class="mb-6 max-w-md px-4 text-center text-sm text-muted sm:text-base">
							Part of the Discover page failed to render. The rest of the app still works.
						</p>
						<Button variant="primary" onclick={() => reset()}>Try Again</Button>
					</div>
				{/snippet}
			</svelte:boundary>
		{/if}
	{/if}
</div>

{#snippet kickerRow(kicker: Kicker | undefined)}
	{#if kicker}
		{@const Icon = kicker.icon}
		<SectionDivider label={kicker.label} updating={sectionsUpdating}>
			{#snippet icon()}<Icon class="h-3.5 w-3.5" />{/snippet}
		</SectionDivider>
	{/if}
{/snippet}

{#snippet blockBody(block: Block, paired: boolean)}
	{#if block.kind === 'queue'}
		<DiscoverQueueDeck youtubeEnabled={discoverData?.integration_status?.youtube ?? false} />
	{:else if block.kind === 'picks' && topPicks}
		<TopPicksDeck section={topPicks} updating={sectionsUpdating} onignore={handleIgnoreTopPick} />
	{:else if block.kind === 'lounge'}
		<ListeningLounge section={lounge} {topPicks} updating={sectionsUpdating} />
	{:else if block.kind === 'weekly' && weekly}
		<WeeklyExploration section={weekly} updating={sectionsUpdating} />
	{:else if block.kind === 'playlist'}
		<DiscoverPlaylistBand covers={bandCovers} onopen={() => (playlistDiscoverOpen = true)} />
	{:else if block.kind === 'mixes'}
		{@render kickerRow(block.kicker)}
		<SectionHead title="Daily Mixes" />
		<div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
			{#each dailyMixes as mix, i (`${mix.title}-${i}`)}
				<DailyMixCard section={mix} />
			{/each}
		</div>
	{:else if block.kind === 'radio'}
		{@render kickerRow(block.kicker)}
		<SectionHead title="Radio Stations" />
		<div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
			{#each radioSections as radio, i (`radio-${radio.radio_seed_id}-${i}`)}
				<RadioCard
					seedType={radio.radio_seed_type ?? 'artist'}
					seedId={radio.radio_seed_id ?? ''}
					initialSection={radio}
				/>
			{/each}
		</div>
	{:else if block.kind === 'because'}
		{#if hasBecauseYouListened}
			{@render kickerRow({ label: 'Because You Listened', icon: Heart })}
			<div class="space-y-5 sm:space-y-6">
				{#each because as entry, i (`${entry.seed_artist_mbid || entry.seed_artist}-${i}`)}
					{#if i === 0}
						<DiscoverArtistHero {entry} />
					{:else}
						<DiscoverArtistMiniBand {entry} />
					{/if}
				{/each}
				{#if artistsYouMightLike || popularInYourGenres}
					<div
						class="dn-discover-row"
						class:dn-discover-row--pair={!!artistsYouMightLike && !!popularInYourGenres}
					>
						{#if artistsYouMightLike}
							<div>
								<HomeSection section={artistsYouMightLike} compact={!!popularInYourGenres} />
							</div>
						{/if}
						{#if popularInYourGenres}
							<div>
								<HomeSection section={popularInYourGenres} compact={!!artistsYouMightLike} />
							</div>
						{/if}
					</div>
				{/if}
			</div>
		{:else}
			<DiscoverBecauseEmpty updating={sectionsUpdating} />
		{/if}
	{:else if block.kind === 'shelf'}
		{@render kickerRow(block.kicker)}
		{#if block.playable}
			<PlayableSection
				section={block.shelf}
				headerLink={block.link ?? null}
				sectionKey={block.key}
				compact={paired}
			/>
		{:else}
			<HomeSection section={block.shelf} compact={paired} />
		{/if}
	{:else if block.kind === 'genres-explore' && unexploredGenres}
		{@render kickerRow(block.kicker)}
		<GenrePills title={unexploredGenres.title} genres={shuffledGenres} onShuffle={shuffleGenres} />
	{:else if block.kind === 'genres-browse' && genreList}
		{@render kickerRow(block.kicker)}
		<div class="dn-discover-genres" class:dn-discover-genres--paired={paired}>
			<GenreGrid title={genreList.title} genres={genreRows(genreList)} />
		</div>
	{:else if block.kind === 'trending-lead'}
		{@render kickerRow({ label: 'Trending Now', icon: TrendingUp })}
		<div class="dn-trending">
			<div class="dn-trending__head">
				<Globe class="size-9 text-title" aria-hidden="true" />
				<h2 class="dn-trending__title">{block.shelf.title}</h2>
				<SourceBadge source={block.shelf.source ?? undefined} />
			</div>
			<div class="min-w-0 flex-1">
				<HomeSection section={block.shelf} hideHeader />
			</div>
		</div>
	{/if}
{/snippet}

<PlaylistDiscoveryModal bind:open={playlistDiscoverOpen} />
<HiddenAlbumsModal bind:open={hiddenAlbumsOpen} />

<style>
	/* the page's column: the zone strip's 80rem, with its side padding */
	.dn-discover-col {
		max-width: 80rem;
		margin-inline: auto;
		padding-inline: 1rem;
	}
	@media (min-width: 768px) {
		.dn-discover-col {
			padding-inline: 1.5rem;
		}
	}
	@media (min-width: 1024px) {
		.dn-discover-col {
			padding-inline: 2rem;
		}
	}

	.dn-discover-rows {
		display: flex;
		flex-direction: column;
		gap: 2.5rem;
		padding-top: 1.5rem;
	}

	/* two half sections share a row on a wide screen; one column on a phone */
	.dn-discover-row {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 2.5rem 2rem;
	}
	@media (min-width: 1024px) {
		.dn-discover-row--pair {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}

	.dn-discover-block {
		min-width: 0;
	}
	/* a section's divider starts the block, so it needs no space above */
	.dn-discover-block > :global(:first-child.flex) {
		padding-top: 0;
	}

	/* the genre tiles beside the pills sit past a rule */
	@media (min-width: 1024px) {
		.dn-discover-block--ruled {
			padding-left: 2rem;
			margin-left: -1rem;
			border-left: 1px solid var(--dn-line);
		}
		.dn-discover-genres--paired :global(.genre-grid) {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}

	/* Globally Trending: its title stacked at the left of the artists */
	.dn-trending {
		display: flex;
		align-items: flex-start;
		gap: 2rem;
	}
	.dn-trending__head {
		display: flex;
		flex: none;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.75rem;
		width: 11rem;
		padding-top: 0.5rem;
	}
	.dn-trending__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 2.5rem;
		line-height: 0.95;
		text-transform: uppercase;
		color: var(--color-base-content);
		font-synthesis: none;
		overflow-wrap: anywhere;
	}
	@media (max-width: 767px) {
		.dn-trending {
			flex-direction: column;
			gap: 0.75rem;
		}
		.dn-trending__head {
			flex-direction: row;
			flex-wrap: wrap;
			align-items: center;
			width: auto;
			padding-top: 0;
		}
		.dn-trending__head :global(svg) {
			width: 1.5rem;
			height: 1.5rem;
		}
		.dn-trending__title {
			font-size: 1.625rem;
		}
	}
</style>
