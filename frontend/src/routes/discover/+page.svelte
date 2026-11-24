<script lang="ts">
	import '$lib/features/discover/components/discover.css';
	import HomeSection from '$lib/features/discover/components/HomeSection.svelte';
	import ServiceErrorState from '$lib/ui/ServiceErrorState.svelte';
	import DailyMixCard from '$lib/features/discover/components/DailyMixCard.svelte';
	import RadioCard from '$lib/features/discover/components/RadioCard.svelte';
	import PlayableSection from '$lib/features/discover/components/PlayableSection.svelte';
	import JumpTabs from '$lib/ui/JumpTabs.svelte';
	import DiscoverHeader from '$lib/features/discover/components/DiscoverHeader.svelte';
	import DiscoverPlaylistBand from '$lib/features/discover/components/DiscoverPlaylistBand.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import GenreGrid from '$lib/features/genre/components/GenreGrid.svelte';
	import DiscoverQueueDeck from '$lib/features/discover/components/DiscoverQueueDeck.svelte';
	import ToldDiscoverModal from '$lib/features/discover/components/ToldDiscoverModal.svelte';
	import PlaylistDiscoveryModal from '$lib/features/discover/components/PlaylistDiscoveryModal.svelte';
	import WeeklyExploration from '$lib/features/discover/components/WeeklyExploration.svelte';
	import ServicePromptCard, {
		promptsFor
	} from '$lib/features/discover/components/ServicePromptCard.svelte';
	import SectionDivider from '$lib/ui/SectionDivider.svelte';
	import CarouselSkeleton from '$lib/ui/CarouselSkeleton.svelte';
	import LiveUpdatingBadge from '$lib/ui/LiveUpdatingBadge.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { isDismissed } from '$lib/features/discover/state/dismissedPrompts';
	import { withBasePath } from '$lib/utils/basePath';
	import SectionStateLine from '$lib/features/discover/components/SectionStateLine.svelte';
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
		SlidersHorizontal
	} from 'lucide-svelte';
	import { getDiscoverHomeV3Query } from '$lib/features/discover/queries/DiscoverV3Queries.svelte';
	import { createRefreshDiscoverV3Mutation } from '$lib/features/discover/queries/DiscoverV3Mutations.svelte';
	import { useDiscoverActivity } from '$lib/features/discover/queries/DiscoverDemand.svelte';
	import type {
		ChartSection,
		PageSection,
		SectionKey,
		SectionState,
		SectionZone
	} from '$lib/features/discover/queries/types';
	import type { KitIcon } from '$lib/ui/Button.svelte';
	import { getSectionPrefsQuery } from '$lib/features/settings/queries/section-prefs.svelte';
	import { shelfOf, shelvesOf, stateOf, weeklyOf } from '$lib/features/discover/queries/sections';
	import { albumRows, genreRows } from '$lib/features/discover/components/sectionRows';

	useDiscoverActivity(() => ({ feature: 'discover' }));

	let playlistDiscoverOpen = $state(false);
	let toldOpen = $state(false);

	const discoverQuery = getDiscoverHomeV3Query();
	const refreshDiscoverMutation = createRefreshDiscoverV3Mutation();
	const sectionPrefsQuery = getSectionPrefsQuery();
	const discoverPrefs = $derived(sectionPrefsQuery.data?.pages?.discover ?? []);
	// client-only chrome: the backend can't blank an action card, so filter here
	const playlistDiscoveryEnabled = $derived(
		discoverPrefs.find((s) => s.key === 'playlist_discovery')?.enabled ?? true
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

	const sections = $derived(discoverData?.sections);
	const weekly = $derived(weeklyOf(sections));
	const dailyMixes = $derived(shelvesOf(sections, 'daily_mixes'));
	const radioSections = $derived(shelvesOf(sections, 'radio_sections'));
	const missingEssentials = $derived(shelfOf(sections, 'missing_essentials'));
	const genreList = $derived(shelfOf(sections, 'genre_list'));

	let hasContent = $derived((sections?.length ?? 0) > 0);
	const sectionStates = $derived(discoverData?.section_states ?? []);

	// A section that is not on the page but says how it stands takes its
	// place in its zone, named and ordered as the section settings list it
	// (in page order, like the page).
	type Entry = { key: SectionKey; zone: SectionZone; section: PageSection | null };
	let entries = $derived.by((): Entry[] => {
		const shown = new Set((sections ?? []).map((section) => section.key));
		const rank = new Map(discoverPrefs.map((pref, i) => [pref.key, i]));
		const absent = sectionStates
			.filter((state) => !shown.has(state.key) && rank.has(state.key))
			.map((state): Entry => ({ key: state.key, zone: state.zone, section: null }));
		const list: Entry[] = [
			...(sections ?? []).map((section) => ({ key: section.key, zone: section.zone, section })),
			...absent
		];
		const at = (key: SectionKey) => rank.get(key) ?? Number.MAX_SAFE_INTEGER;
		return list.sort((a, b) => at(a.key) - at(b.key));
	});
	const titleOf = (key: SectionKey) => discoverPrefs.find((p) => p.key === key)?.title ?? '';
	let isBuilding = $derived(
		!!discoverData && !hasContent && (!!discoverData.refreshing || isUpdating)
	);
	let isUpdatingInBackground = $derived(!!discoverData?.refreshing && hasContent);
	const sectionsUpdating = $derived(!!discoverData?.refreshing);
	let dismissVersion = $state(0);
	let servicePrompts = $derived.by(() => {
		void dismissVersion;
		return promptsFor(discoverData?.service_prompts ?? [], authStore.isAdmin).filter(
			(p) => !isDismissed(p.service)
		);
	});

	let queueEnabled = $derived(discoverData?.weekly_set_enabled ?? true);

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
		/** How the section stands, when it is not plainly built. */
		state?: SectionState;
	} & (
		| { kind: 'queue' | 'weekly' | 'playlist' }
		| { kind: 'mixes' | 'radio' | 'genres-browse' }
		| { kind: 'shelf'; shelf: ChartSection; key: SectionKey; playable: boolean; link?: string }
		| { kind: 'state'; title: string; state: SectionState }
	);

	const ZONES: Partial<Record<SectionZone, { id: string; nav: string } & Kicker>> = {
		made_for_you: { id: 'zone-made', nav: 'Made For You', label: 'Made For You', icon: Sparkles },
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

	// zones that come before the playlist band
	const EARLY_ZONES = new Set<SectionZone>(['essentials', 'for_you']);

	let blocks = $derived.by(() => {
		const list: Block[] = [];
		const seenZones: SectionZone[] = [];
		let playlistPlaced = false;
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

		if (queueEnabled)
			list.push({
				id: 'zone-queue',
				kind: 'queue',
				width: 'wide',
				nav: { id: 'zone-queue', label: 'Queue' }
			});

		for (const { key, zone, section } of entries) {
			if (!EARLY_ZONES.has(zone)) placePlaylist();

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
			const state = stateOf(sectionStates, key) ?? undefined;

			if (!section) {
				if (state) place({ id, kind: 'state', width: 'half', kicker, title: titleOf(key), state });
				continue;
			}
			const { content } = section;
			const placed = list.length;

			if (key === 'weekly_exploration') {
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
			} else if (key === 'genre_list') {
				if (genreList) place({ id, kind: 'genres-browse', width: 'half', kicker });
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
			if (state && list.length > placed) list[placed].state = state;
		}
		placePlaylist();
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
		const ids = albumRows(missingEssentials)
			.map((album) => album.musicbrainz_id)
			.filter((id): id is string => !!id);
		return ids.filter((id, i) => ids.indexOf(id) === i).slice(0, 4);
	});

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
					</div>
				{/if}
				<div class="space-y-8" aria-label="Loading Discover sections">
					{#each ['Made For You', 'New & Fresh'] as label (label)}
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
							title="What you told Discover"
							onclick={() => (toldOpen = true)}
						>
							<span class="hidden sm:inline">What you told Discover</span>
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
									{#if block.state && block.kind !== 'state'}
										<SectionStateLine status={block.state} />
									{/if}
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
	{:else if block.kind === 'weekly' && weekly}
		<WeeklyExploration madeFrom="discover_shelf" section={weekly} updating={sectionsUpdating} />
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
			<HomeSection madeFrom="discover_shelf" section={block.shelf} compact={paired} />
		{/if}
	{:else if block.kind === 'genres-browse' && genreList}
		{@render kickerRow(block.kicker)}
		<div class="dn-discover-genres" class:dn-discover-genres--paired={paired}>
			<GenreGrid title={genreList.title} genres={genreRows(genreList)} />
		</div>
	{:else if block.kind === 'state'}
		{@render kickerRow(block.kicker)}
		<SectionHead title={block.title} />
		<SectionStateLine status={block.state} />
	{/if}
{/snippet}

<PlaylistDiscoveryModal bind:open={playlistDiscoverOpen} />
<ToldDiscoverModal bind:open={toldOpen} />

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
</style>
