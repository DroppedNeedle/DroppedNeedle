<script lang="ts">
	import { Download, Music, CircleAlert, Lock, Pencil, Plus } from 'lucide-svelte';
	import HomeSection from '$lib/features/discover/components/HomeSection.svelte';
	import ServiceErrorState from '$lib/ui/ServiceErrorState.svelte';
	import WeeklyExploration from '$lib/features/discover/components/WeeklyExploration.svelte';
	import ServicePromptCard, {
		promptsFor
	} from '$lib/features/discover/components/ServicePromptCard.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import GenreGrid from '$lib/features/genre/components/GenreGrid.svelte';
	import LiveUpdatingBadge from '$lib/ui/LiveUpdatingBadge.svelte';
	import type {
		ChartSection as HomeSectionType,
		SectionKey,
		SectionZone,
		WeeklyExploration as WeeklyExplorationSectionType
	} from '$lib/features/discover/queries/types';
	import { shelfOf, zoneSections } from '$lib/features/discover/queries/sections';
	import CarouselSkeleton from '$lib/ui/CarouselSkeleton.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { genreRows } from '$lib/features/discover/components/sectionRows';
	import { formatLastUpdated, getGreeting } from '$lib/utils/formatting';
	import { isDismissed } from '$lib/features/discover/state/dismissedPrompts';
	import HomeSectionNowPlaying from '$lib/features/home/components/HomeSectionNowPlaying.svelte';
	import HomeEntryCards from '$lib/features/home/components/HomeEntryCards.svelte';
	import DiscoverTeaserBand from '$lib/features/discover/components/DiscoverTeaserBand.svelte';
	import { getHomeQuery } from '$lib/features/home/queries/home.svelte';
	import { useDiscoverActivity } from '$lib/features/discover/queries/DiscoverDemand.svelte';
	import { nowPlayingMerged } from '$lib/features/player/state/nowPlayingMerged.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import HomeGrid from '$lib/features/home/components/HomeGrid.svelte';
	import HomeWidget from '$lib/features/home/components/HomeWidget.svelte';
	import { defaultLayout } from '$lib/features/home/layout/defaultLayout';
	import { LAYOUT_VERSION, applySavedLayout } from '$lib/features/home/layout/apply';
	import { getHomeLayoutQuery, saveHomeLayout } from '$lib/features/home/queries/layout.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { WidgetPlacement, WidgetSpec } from '$lib/features/home/layout/contract';

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

	// rows each shelf needs at the measured unit; artists sit shorter than albums,
	// the track strip shorter still
	const SHELF_ROWS: Partial<Record<SectionKey, number>> = {
		trending_artists: 3,
		library_artists: 3,
		recently_played: 2
	};

	const SHELF_MIN_ROWS: Partial<Record<SectionKey, number>> = { recently_played: 2 };

	// the first two shelves are above the fold and load their art eagerly
	const EAGER_KEYS: SectionKey[] = ['popular_albums', 'trending_artists'];

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
	let servicePrompts = $derived(promptsFor(homeData?.service_prompts || [], authStore.isAdmin));
	let downloadClientConfigured = $derived(homeData?.integration_status?.download_client ?? true);
	let downloadClientPrompt = $derived(servicePrompts.find((p) => p.service === 'download-client'));
	// the welcome card stands in for the other cards until a download source is set
	let welcome = $derived(!downloadClientConfigured && !!downloadClientPrompt);
	let nowPlayingSessions = $derived(nowPlayingMerged.sessions);

	const getOtherPrompts = () => {
		return servicePrompts.filter((p) => p.service !== 'download-client' && !isDismissed(p.service));
	};
	let otherPrompts = $derived(getOtherPrompts());

	function handlePromptDismiss(_service: string) {
		otherPrompts = getOtherPrompts();
	}

	function shelfSpec(block: Block): WidgetSpec {
		return {
			id: `shelf:${block.key}`,
			kind: block.kind === 'weekly' ? 'weekly' : 'shelf',
			height: SHELF_ROWS[block.key],
			minH: SHELF_MIN_ROWS[block.key]
		};
	}

	// page order, with empty blocks left out
	const widgetSpecs = $derived.by((): WidgetSpec[] => {
		const specs: WidgetSpec[] = [{ id: 'entry_cards', kind: 'entry_cards' }];
		if (welcome || otherPrompts.length > 0) specs.push({ id: 'prompts', kind: 'prompts' });
		if (nowPlayingSessions.length > 0) specs.push({ id: 'now_playing', kind: 'now_playing' });
		for (const block of whatsHotBlocks) specs.push(shelfSpec(block));
		specs.push({ id: 'discover_teaser', kind: 'discover_teaser' });
		for (const block of forYouBlocks) specs.push(shelfSpec(block));
		if (genreList) specs.push({ id: 'genre_grid', kind: 'genre_grid' });
		for (const block of postGenreSections) specs.push(shelfSpec(block));
		return specs;
	});

	const labels = $derived.by(() => {
		const byId: Record<string, string> = {
			entry_cards: 'Your music',
			prompts: 'Setup',
			now_playing: 'Now playing',
			discover_teaser: 'Picked for you',
			genre_grid: 'Genres'
		};
		for (const block of [...whatsHotBlocks, ...forYouBlocks, ...postGenreSections]) {
			byId[`shelf:${block.key}`] = block.kind === 'section' ? block.section.title : 'Weekly';
		}
		return byId;
	});

	const layoutQuery = getHomeLayoutQuery();
	const serverLayout = $derived(layoutQuery.data);
	const editable = $derived(serverLayout?.editable ?? true);

	let layout = $state<WidgetPlacement[]>([]);
	let hidden = $state<string[]>([]);
	let editing = $state(false);
	let confirming = $state<'revert' | 'reset' | null>(null);
	// locking saves here; revert returns to it
	let saved = $state<{ layout: WidgetPlacement[]; hidden: string[] }>({ layout: [], hidden: [] });

	// svelte state is a proxy; copy through it, structuredClone rejects proxies
	const copy = (places: WidgetPlacement[]) => places.map((place) => ({ ...place }));

	// the saved layout fills in once both the page data and the layout land;
	// nothing saved, or an unreachable endpoint, means the built-in order
	let seeded = false;
	$effect(() => {
		if (seeded || !homeData) return;
		if (!layoutQuery.data && !layoutQuery.isError) return;
		seeded = true;
		const stored = layoutQuery.data;
		layout =
			stored && stored.widgets.length > 0
				? applySavedLayout(stored.widgets, widgetSpecs, stored.source !== 'admin')
				: defaultLayout(widgetSpecs);
		hidden = (stored?.hidden ?? []).filter((id) => widgetSpecs.some((spec) => spec.id === id));
		saved = { layout: copy(layout), hidden: [...hidden] };
	});

	// a section that appears or vanishes on a refetch joins or leaves the layout
	$effect(() => {
		if (!seeded || serverLayout?.source === 'admin') return;
		const ids = new Set(widgetSpecs.map((spec) => spec.id));
		const known = new Set(layout.map((place) => place.id));
		const missing = widgetSpecs.filter((spec) => !known.has(spec.id));
		const stale = [...known].some((id) => !ids.has(id));
		if (missing.length === 0 && !stale) return;
		const kept = layout.filter((place) => ids.has(place.id));
		const bottom = kept.reduce((low, place) => Math.max(low, place.y + place.h), 0);
		layout = [
			...kept,
			...defaultLayout(missing).map((place) => ({ ...place, y: place.y + bottom }))
		];
		if (stale) hidden = hidden.filter((id) => ids.has(id));
	});

	const visibleWidgets = $derived(layout.filter((place) => !hidden.includes(place.id)));
	const hiddenSpecs = $derived(widgetSpecs.filter((spec) => hidden.includes(spec.id)));

	function unlock() {
		if (!editable) {
			toastStore.notify('The admin sets the home layout for everyone right now.', 'info');
			return;
		}
		saved = { layout: copy(layout), hidden: [...hidden] };
		confirming = null;
		editing = true;
	}

	async function lock() {
		editing = false;
		confirming = null;
		saved = { layout: copy(layout), hidden: [...hidden] };
		try {
			await saveHomeLayout({
				version: LAYOUT_VERSION,
				widgets: layout.map(({ id, kind, x, y, w, h }) => ({ id, kind, x, y, w, h })),
				hidden: [...hidden]
			});
		} catch {
			toastStore.notify('Could not save the home layout.', 'error');
		}
	}

	function revert() {
		layout = copy(saved.layout);
		hidden = [...saved.hidden];
		confirming = null;
	}

	function reset() {
		layout = defaultLayout(widgetSpecs);
		hidden = [];
		confirming = null;
	}

	function hideWidget(id: string) {
		if (!hidden.includes(id)) hidden = [...hidden, id];
	}

	function showWidget(id: string) {
		const bottom = layout.reduce((low, place) => Math.max(low, place.y + place.h), 0);
		layout = layout.map((place) => (place.id === id ? { ...place, y: bottom } : place));
		hidden = hidden.filter((each) => each !== id);
	}

	function handleLayoutChange(moved: { id: string; x: number; y: number; w: number; h: number }[]) {
		layout = layout.map((place) => {
			const next = moved.find((each) => each.id === place.id);
			return next ? { ...place, ...next } : place;
		});
	}

	const blocksById = $derived.by(() => {
		const byId: Record<string, Block> = {};
		for (const block of [...whatsHotBlocks, ...forYouBlocks, ...postGenreSections]) {
			byId[`shelf:${block.key}`] = block;
		}
		return byId;
	});
</script>

<svelte:head>
	<title>Home - DroppedNeedle</title>
</svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<header
		class="flex flex-col gap-4 px-4 pt-8 pb-6 sm:px-6 sm:pt-10 sm:pb-8 md:flex-row md:items-start md:justify-between lg:px-8"
	>
		<div class="min-w-0">
			<h1 class="greeting font-display text-title">
				<Music class="greeting-note" aria-hidden="true" />
				{getGreeting()}
			</h1>
			<p class="mt-3 max-w-xl text-base text-muted">
				Discover music, explore your library, and find new favorites.
			</p>
		</div>
		<div class="flex shrink-0 items-center justify-between gap-4 md:flex-col md:items-end md:pt-2">
			{#if isUpdating || homeData?.refreshing}
				<LiveUpdatingBadge label="Refreshing" />
			{:else if lastUpdated && !loading}
				<span class="header-meta text-muted">Updated {formatLastUpdated(lastUpdated)}</span>
			{/if}
			<div class="edit-bar header-meta ml-auto md:ml-0">
				{#if editing}
					{#if confirming === 'revert'}
						<span class="edit-question">Revert to the saved layout?</span>
						<button type="button" class="edit-btn" onclick={revert}>Revert</button>
						<button type="button" class="edit-btn" onclick={() => (confirming = null)}>
							Cancel
						</button>
					{:else if confirming === 'reset'}
						<span class="edit-question">Reset to the default layout?</span>
						<button type="button" class="edit-btn" onclick={reset}>Reset</button>
						<button type="button" class="edit-btn" onclick={() => (confirming = null)}>
							Cancel
						</button>
					{:else}
						<button type="button" class="edit-btn" onclick={() => (confirming = 'revert')}>
							Revert
						</button>
						<button type="button" class="edit-btn" onclick={() => (confirming = 'reset')}>
							Reset
						</button>
						<button type="button" class="edit-btn edit-btn--primary" onclick={lock}>
							<Lock class="size-3.5" aria-hidden="true" /> Lock
						</button>
					{/if}
				{:else}
					<button type="button" class="edit-btn" onclick={unlock}>
						<Pencil class="size-3.5" aria-hidden="true" /> Edit layout
					</button>
				{/if}
			</div>
		</div>
	</header>

	{#if homeQuery.error && !homeData}
		<ServiceErrorState error={homeQuery.error} what="Home" onretry={() => homeQuery.refetch()} />
	{:else if loading && !homeData}
		<div class="space-y-6 px-4 sm:space-y-8 sm:px-6 lg:px-8">
			<HomeEntryCards />
			<section>
				<div class="skeleton skeleton-shimmer mb-4 h-6 w-40"></div>
				<CarouselSkeleton />
			</section>
			<section>
				<div class="skeleton skeleton-shimmer mb-4 h-6 w-36"></div>
				<div class="grid grid-cols-2 gap-2 sm:grid-cols-3 sm:gap-3 md:grid-cols-4 lg:grid-cols-5">
					{#each Array(10) as _, i (`genre-skeleton-${i}`)}
						<div class="skeleton skeleton-shimmer h-20 rounded-lg sm:h-24"></div>
					{/each}
				</div>
			</section>
		</div>
	{:else}
		<!-- a render error in any widget degrades to the failed card below instead
		     of crashing the whole SPA (the #147 freeze) -->
		<svelte:boundary>
			<div class="relative px-4 sm:px-6 lg:px-8">
				{#if editing}
					<p class="edit-hint">
						Drag a widget by its grip, resize from the corner, hide what you do not need.
					</p>
				{/if}
				<HomeGrid widgets={visibleWidgets} {editing} onchange={handleLayoutChange}>
					{#snippet children(widget)}
						<HomeWidget
							id={widget.id}
							label={labels[widget.id] ?? widget.kind}
							{editing}
							onhide={hideWidget}
						>
							{#if widget.kind === 'entry_cards'}
								<HomeEntryCards />
							{:else if widget.kind === 'prompts'}
								<div class="space-y-3">
									{#if welcome && downloadClientPrompt}
										<Card
											class="relative overflow-hidden border-2 border-accent/40 bg-linear-to-br from-accent/20 via-accent/10 to-base-200 shadow-xl"
										>
											<div
												class="flex flex-col items-center gap-2 py-6 text-center stagger-fade-in"
											>
												<Music class="h-16 w-16 mb-4 animate-float text-accent" />
												<CardTitle class="block text-2xl sm:text-4xl lg:text-5xl font-bold mb-2">
													Welcome to <span class="text-primary">DroppedNeedle</span>!
												</CardTitle>
												<p class="text-muted max-w-lg mb-6">
													Connect a download source to fetch your requests. Until then, requests
													wait.
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
									{#if otherPrompts.length > 0 && !welcome}
										{#each otherPrompts as prompt, i (`prompt-${i}`)}
											<ServicePromptCard {prompt} ondismiss={handlePromptDismiss} />
										{/each}
									{/if}
								</div>
							{:else if widget.kind === 'now_playing'}
								<HomeSectionNowPlaying />
							{:else if widget.kind === 'discover_teaser'}
								<div class="grid h-full items-center">
									<DiscoverTeaserBand preview={homeData?.discover_preview ?? null} />
								</div>
							{:else if widget.kind === 'genre_grid' && genreList}
								<GenreGrid title={genreList.title} genres={genreRows(genreList)} />
							{:else}
								{@const block = blocksById[widget.id]}
								{#if block?.kind === 'section'}
									<HomeSection
										section={block.section}
										headerLink={block.link}
										showPreview={block.showPreview}
										eager={EAGER_KEYS.includes(block.key)}
										madeFrom="home_shelf"
									/>
								{:else if block?.kind === 'weekly'}
									<WeeklyExploration section={block.section} madeFrom="home_shelf" />
								{/if}
							{/if}
						</HomeWidget>
					{/snippet}
				</HomeGrid>

				{#if editing && hiddenSpecs.length > 0}
					<div class="home-tray">
						<span class="home-tray-label">Hidden</span>
						{#each hiddenSpecs as spec (spec.id)}
							<button type="button" class="home-tray-add" onclick={() => showWidget(spec.id)}>
								<Plus class="size-3.5" aria-hidden="true" />
								{labels[spec.id] ?? spec.kind}
							</button>
						{/each}
					</div>
				{/if}
			</div>

			{#if !hasContent && servicePrompts.length === 0}
				<div class="flex flex-col items-center justify-center py-12 sm:py-16">
					<Music class="h-12 w-12 sm:h-16 sm:w-16 mb-4 sm:mb-6" />
					<h2 class="mb-2 text-center text-3xl font-bold sm:text-4xl lg:text-5xl">
						Welcome to <span class="text-primary">DroppedNeedle</span>
					</h2>
					{#if authStore.isAdmin}
						<p class="mb-6 max-w-md px-4 text-center text-sm text-muted sm:text-base">
							Your library is empty. Add a library path and start a scan to fill it.
						</p>
						<div class="flex flex-wrap justify-center gap-2">
							<Button variant="primary" href={withBasePath('/settings?tab=library')}>
								Start scan
							</Button>
							<Button variant="ghost" href={withBasePath('/library')}>Go to Library</Button>
						</div>
					{:else}
						<p class="mb-6 max-w-md px-4 text-center text-sm text-muted sm:text-base">
							Your library is being prepared. An admin is setting things up - check back soon.
						</p>
						<Button variant="ghost" href={withBasePath('/library')}>Go to Library</Button>
					{/if}
				</div>
			{/if}

			{#snippet failed(_error, reset)}
				<div class="flex flex-col items-center justify-center py-12 sm:py-16">
					<CircleAlert class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-muted" />
					<h2 class="mb-2 text-center text-xl font-bold sm:text-2xl">Something Went Wrong</h2>
					<p class="mb-6 max-w-md px-4 text-center text-sm text-muted sm:text-base">
						Part of the Home page failed to render. The rest of the app still works.
					</p>
					<Button variant="primary" onclick={() => reset()}>Try Again</Button>
				</div>
			{/snippet}
		</svelte:boundary>
	{/if}
</div>

<style>
	.greeting {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		font-size: clamp(3.5rem, 18vw, 5.5rem);
		line-height: 0.92;
	}
	.greeting :global(.greeting-note) {
		display: none;
		width: 0.6em;
		height: 0.6em;
		flex-shrink: 0;
	}
	@media (min-width: 768px) {
		.greeting :global(.greeting-note) {
			display: block;
		}
	}

	.header-meta {
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
	.edit-bar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem;
	}
	.edit-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		min-height: 2rem;
		padding: 0 0.5rem;
		border: 1px solid var(--dn-edge);
		border-radius: 0.25rem;
		font: inherit;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
	.edit-btn:hover {
		background: var(--dn-action);
		color: var(--dn-action-content);
	}
	.edit-btn--primary {
		border-color: var(--dn-action);
		background: var(--dn-action);
		color: var(--dn-action-content);
	}
	.edit-question {
		color: var(--color-base-content);
	}
	/* sits in the header's bottom padding, so unlocking moves nothing */
	.edit-hint {
		position: absolute;
		top: -1.45rem;
		left: 1rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.04em;
		color: var(--color-base-content);
		opacity: 0.72;
	}
	@media (min-width: 640px) {
		.edit-hint {
			left: 1.5rem;
		}
	}
	@media (min-width: 1024px) {
		.edit-hint {
			left: 2rem;
		}
	}

	.home-tray {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem;
		margin-top: 1.25rem;
		padding: 0.75rem;
		border: 1px dashed var(--dn-edge);
		border-radius: 0.375rem;
	}
	.home-tray-label {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		opacity: 0.72;
	}
	.home-tray-add {
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		min-height: 2rem;
		padding: 0 0.625rem;
		border: 1px solid var(--dn-edge);
		border-radius: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
	.home-tray-add:hover {
		background: var(--dn-action);
		color: var(--dn-action-content);
	}
</style>
