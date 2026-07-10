<script lang="ts">
	import { page } from '$app/state';
	import { replaceState } from '$app/navigation';
	import { onMount } from 'svelte';
	import { fromStore } from 'svelte/store';
	import { integrationStore } from '$lib/stores/integration';
	import SettingsPreferences from '$lib/components/settings/SettingsPreferences.svelte';
	import SettingsCache from '$lib/components/settings/SettingsCache.svelte';
	import SettingsLibrary from '$lib/components/settings/SettingsLibrary.svelte';
	import SettingsJellyfin from '$lib/components/settings/SettingsJellyfin.svelte';
	import SettingsNavidrome from '$lib/components/settings/SettingsNavidrome.svelte';
	import SettingsPlex from '$lib/components/settings/SettingsPlex.svelte';
	import SettingsYouTube from '$lib/components/settings/SettingsYouTube.svelte';
	import SettingsLastFmApp from '$lib/components/settings/SettingsLastFmApp.svelte';
	import SettingsMusicSource from '$lib/components/settings/SettingsMusicSource.svelte';
	import SettingsAdvanced from '$lib/components/settings/SettingsAdvanced.svelte';
	import SettingsMusicBrainz from '$lib/components/settings/SettingsMusicBrainz.svelte';
	import SettingsAbout from '$lib/components/settings/SettingsAbout.svelte';
	import SettingsHome from '$lib/components/settings/SettingsHome.svelte';
	import SettingsDiscover from '$lib/components/settings/SettingsDiscover.svelte';
	import SettingsUsers from '$lib/components/settings/SettingsUsers.svelte';
	import SettingsSecurity from '$lib/components/settings/SettingsSecurity.svelte';
	import SettingsDownloadClient from '$lib/components/settings/SettingsDownloadClient.svelte';
	import SettingsSabnzbd from '$lib/components/settings/SettingsSabnzbd.svelte';
	import SettingsSourcePriority from '$lib/components/settings/SettingsSourcePriority.svelte';
	import SettingsDownloadPolicy from '$lib/components/settings/SettingsDownloadPolicy.svelte';
	import SettingsWanted from '$lib/components/settings/SettingsWanted.svelte';
	import SettingsIndexers from '$lib/components/settings/SettingsIndexers.svelte';
	import SettingsLidarrImport from '$lib/components/settings/SettingsLidarrImport.svelte';
	import SettingsConnectApps from '$lib/components/settings/SettingsConnectApps.svelte';
	import SettingsOnboardingChecklist from '$lib/components/settings/SettingsOnboardingChecklist.svelte';
	import SettingsSpotify from '$lib/components/settings/SettingsSpotify.svelte';
	import SettingsEvents from '$lib/components/settings/SettingsEvents.svelte';
	import SettingsFreeMusic from '$lib/components/settings/SettingsFreeMusic.svelte';
	import SettingsGetIt from '$lib/components/settings/SettingsGetIt.svelte';
	import SettingsPlugins from '$lib/components/settings/SettingsPlugins.svelte';
	import SettingsWrapped from '$lib/components/settings/SettingsWrapped.svelte';
	import SettingsAppearance from '$lib/components/settings/SettingsAppearance.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getUpdateCheckQuery } from '$lib/queries/VersionQuery.svelte';
	import {
		Settings2,
		Music,
		Youtube,
		Database,
		Settings,
		Radio,
		Search,
		BarChart3,
		Info,
		ArrowUpCircle,
		Globe,
		Home,
		Compass,
		Users,
		ShieldCheck,
		HardDriveDownload,
		Waypoints,
		CalendarClock,
		DownloadCloud,
		Gift,
		ShoppingBag,
		Landmark,
		Blocks,
		Palette,
		Puzzle,
		ChevronRight,
		ChevronLeft
	} from 'lucide-svelte';
	import PageHero from '$lib/ui/PageHero.svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import SpotifyIcon from '$lib/components/SpotifyIcon.svelte';

	const integration = fromStore(integrationStore);

	const updateCheckQuery = getUpdateCheckQuery();
	const updateAvailable = $derived(updateCheckQuery.data?.update_available ?? false);

	const connectionMap: Record<
		string,
		'jellyfin' | 'navidrome' | 'plex' | 'youtube' | 'localfiles'
	> = {
		jellyfin: 'jellyfin',
		navidrome: 'navidrome',
		plex: 'plex',
		youtube: 'youtube'
	};

	let activeTab = $state('overview');
	let filter = $state('');

	const tiers = [
		{ id: 'setup', label: 'Setup', hint: 'Connect your sources' },
		{ id: 'personalize', label: 'Personalize', hint: 'Tune your content' },
		{ id: 'system', label: 'System', hint: 'Maintenance & account' }
	];

	const tabs = [
		{
			id: 'library',
			label: 'Library',
			tier: 'setup',
			icon: Music,
			hint: 'Paths, scanning & naming'
		},
		...(authStore.isAdmin
			? [
					{ id: 'free-music', label: 'Free Music', tier: 'setup', icon: Landmark },
					{
						id: 'download-client',
						label: 'Download Client',
						tier: 'setup',
						icon: HardDriveDownload,
						hint: 'Soulseek, Usenet & policy'
					},
					{
						id: 'indexers',
						label: 'Indexers',
						tier: 'setup',
						icon: Search,
						hint: 'Newznab sources'
					},
					{
						id: 'lidarr-import',
						label: 'Lidarr Import',
						tier: 'setup',
						icon: DownloadCloud,
						hint: 'Bring your follows over'
					}
				]
			: []),
		{
			id: 'connect-apps',
			label: 'Connect Apps',
			tier: 'setup',
			icon: Waypoints,
			hint: 'Stream in Symfonium & more'
		},
		{
			id: 'jellyfin',
			label: 'Jellyfin',
			tier: 'setup',
			icon: JellyfinIcon,
			hint: 'Server & login'
		},
		{
			id: 'navidrome',
			label: 'Navidrome',
			tier: 'setup',
			icon: NavidromeIcon,
			hint: 'Server connection'
		},
		{ id: 'plex', label: 'Plex', tier: 'setup', icon: PlexIcon, hint: 'Server, libraries & login' },
		{ id: 'youtube', label: 'YouTube', tier: 'setup', icon: Youtube, hint: 'Playback & API quota' },
		...(authStore.isAdmin
			? [{ id: 'lastfm', label: 'Last.fm', tier: 'setup', icon: Radio, hint: 'App credentials' }]
			: []),
		...(authStore.isAdmin
			? [
					{
						id: 'spotify',
						label: 'Spotify',
						tier: 'setup',
						icon: SpotifyIcon,
						hint: 'App credentials'
					}
				]
			: []),
		...(authStore.isAdmin
			? [
					{
						id: 'events',
						label: 'Live Events',
						tier: 'setup',
						icon: CalendarClock,
						hint: 'Concert providers'
					}
				]
			: []),
		...(authStore.isAdmin
			? [
					{
						id: 'get-it',
						label: 'Get it',
						tier: 'setup',
						icon: ShoppingBag,
						hint: 'Where-to-buy links'
					}
				]
			: []),
		{
			id: 'appearance',
			label: 'Appearance',
			tier: 'personalize',
			icon: Palette,
			hint: 'Theme, text size & motion'
		},
		{
			id: 'settings',
			label: 'Release Types',
			tier: 'personalize',
			icon: Settings2,
			hint: 'What counts as an album'
		},
		{ id: 'home', label: 'Home', tier: 'personalize', icon: Home, hint: 'Choose your sections' },
		{
			id: 'discover',
			label: 'Discover',
			tier: 'personalize',
			icon: Compass,
			hint: 'Choose your zones'
		},
		{
			id: 'music-source',
			label: 'Music Source',
			tier: 'personalize',
			icon: BarChart3,
			hint: 'ListenBrainz or Last.fm'
		},
		{ id: 'cache', label: 'Cache', tier: 'system', icon: Database, hint: 'Storage & cleanup' },
		{
			id: 'musicbrainz',
			label: 'MusicBrainz',
			tier: 'system',
			icon: Globe,
			hint: 'Metadata endpoint'
		},
		...(authStore.isAdmin
			? [
					{
						id: 'users',
						label: 'Users',
						tier: 'system',
						icon: Users,
						hint: 'Accounts, roles & quotas'
					},
					{
						id: 'security',
						label: 'Security',
						tier: 'system',
						icon: ShieldCheck,
						hint: 'Passwords, HSTS & SSO'
					},
					{
						id: 'wrapped',
						label: 'Wrapped API',
						tier: 'system',
						icon: Gift,
						hint: 'Year-in-review key'
					},
					{
						id: 'plugins',
						label: 'Plugins',
						tier: 'system',
						icon: Puzzle,
						hint: 'Acquisition sources'
					}
				]
			: []),
		{
			id: 'advanced',
			label: 'Advanced',
			tier: 'system',
			icon: Settings,
			hint: 'Caches, batching & tuning'
		},
		{ id: 'about', label: 'About', tier: 'system', icon: Info, hint: 'Version & release notes' }
	];

	const activeTabMeta = $derived(tabs.find((t) => t.id === activeTab));

	const normalizedFilter = $derived(filter.trim().toLowerCase());
	function tabsForTier(tier: string) {
		return tabs.filter(
			(t) =>
				t.tier === tier && (!normalizedFilter || t.label.toLowerCase().includes(normalizedFilter))
		);
	}
	const noMatches = $derived(
		normalizedFilter.length > 0 && tiers.every((t) => tabsForTier(t.id).length === 0)
	);

	function selectTab(id: string) {
		activeTab = id;
		// drill-down navigation on phones: land at the top of the panel/list
		if (typeof window !== 'undefined' && window.innerWidth < 1024) {
			window.scrollTo({ top: 0 });
		}
		const url = new URL(page.url);
		if (id === 'overview') {
			url.searchParams.delete('tab');
		} else {
			url.searchParams.set('tab', id);
		}
		replaceState(url, {});
	}

	onMount(() => {
		integrationStore.ensureLoaded();
	});

	$effect(() => {
		const tabParam = page.url.searchParams.get('tab');
		if (tabParam && tabs.some((t) => t.id === tabParam)) {
			activeTab = tabParam;
		} else if (!tabParam) {
			activeTab = 'overview';
		}
	});
</script>

<div class="min-h-screen bg-base-100">
	<PageHero
		title="Control Center"
		subtitle="Everything behind the music — sources, personalization, and system care."
		eyebrow="Settings"
		tint="var(--color-primary)"
	>
		{#snippet icon()}
			<Settings class="h-7 w-7" />
		{/snippet}
	</PageHero>

	<div class="container mx-auto px-4 pb-10 max-w-7xl">
		{#if activeTab === 'overview'}
			<!-- control-center landing: every category at a glance -->
			<label class="relative mb-6 block max-w-md">
				<Search class="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-base-content/40" />
				<input
					type="text"
					bind:value={filter}
					placeholder="Search settings…"
					class="input input-soft w-full pl-9"
				/>
			</label>

			<div class="space-y-8">
				{#each tiers as tier (tier.id)}
					{@const tierTabs = tabsForTier(tier.id)}
					{#if tierTabs.length > 0}
						<section>
							<h2 class="mb-1 text-sm font-bold uppercase tracking-widest text-base-content/50">
								{tier.label}
							</h2>
							<p class="mb-3 text-xs text-base-content/35">{tier.hint}</p>
							<div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
								{#each tierTabs as tab (tab.id)}
									{@const Icon = tab.icon}
									<button
										class="group flex items-start gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-4 text-left transition-all hover:-translate-y-0.5 hover:border-primary/30 hover:bg-base-200 hover:shadow-lg"
										onclick={() => selectTab(tab.id)}
									>
										<span
											class="mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-base-content/6 text-base-content/70 transition-colors group-hover:bg-primary/12 group-hover:text-primary"
										>
											<Icon class="h-4.5 w-4.5" />
										</span>
										<span class="min-w-0 flex-1">
											<span class="flex items-center gap-2">
												<span class="truncate font-semibold">{tab.label}</span>
												{#if tab.id in connectionMap}
													{@const connected = integration.current[connectionMap[tab.id]]}
													<span
														class="h-2 w-2 shrink-0 rounded-full {connected
															? 'bg-success ring-2 ring-success/30'
															: 'bg-base-content/20'}"
													>
														<span class="sr-only">{connected ? 'Connected' : 'Not connected'}</span>
													</span>
												{/if}
												{#if tab.id === 'about' && updateAvailable}
													<span
														class="flex shrink-0 items-center gap-1 rounded-full bg-accent/15 px-2 py-0.5 text-[0.65rem] font-semibold text-accent"
													>
														<ArrowUpCircle class="h-3 w-3" />
														Update
													</span>
												{/if}
											</span>
											<span class="mt-0.5 block truncate text-xs text-base-content/50"
												>{tab.hint}</span
											>
										</span>
									</button>
								{/each}
							</div>
						</section>
					{/if}
				{/each}
				{#if noMatches}
					<p class="px-1 py-2 text-sm text-base-content/40">No settings match "{filter}".</p>
				{/if}
			</div>
		{:else}
			<!-- mobile: drill-down header (list → panel, like a native settings app) -->
			<div class="dn-settings-mobilehead lg:hidden">
				<button
					class="btn btn-ghost btn-sm gap-1.5 rounded-full bg-base-content/6 pl-2.5"
					onclick={() => selectTab('overview')}
				>
					<ChevronLeft class="h-4 w-4" />
					All settings
				</button>
				<h2 class="hero-title min-w-0 flex-1 truncate text-right font-display text-lg font-bold">
					{activeTabMeta?.label ?? activeTab}
				</h2>
			</div>

			<!-- desktop: breadcrumb -->
			<nav class="mb-5 hidden items-center gap-2 text-sm lg:flex" aria-label="Breadcrumb">
				<button
					class="link-hover font-medium text-base-content/55 hover:text-base-content"
					onclick={() => selectTab('overview')}
				>
					All settings
				</button>
				<ChevronRight class="h-3.5 w-3.5 text-base-content/30" />
				<span class="font-semibold">{activeTabMeta?.label ?? activeTab}</span>
			</nav>

			<div class="dn-settings-body flex flex-col lg:flex-row gap-6">
				<aside
					class="scrollbar-hide hidden w-full space-y-3 lg:sticky lg:top-20 lg:block lg:max-h-[calc(100vh-6rem)] lg:w-80 lg:shrink-0 lg:self-start lg:overflow-y-auto"
				>
					<label class="relative block">
						<Search class="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-base-content/40" />
						<input
							type="text"
							bind:value={filter}
							placeholder="Filter settings…"
							class="input input-sm input-soft w-full pl-9"
						/>
					</label>

					{#each tiers as tier (tier.id)}
						{@const tierTabs = tabsForTier(tier.id)}
						{#if tierTabs.length > 0}
							<div
								class="rounded-2xl border p-2 {tier.id === 'setup'
									? 'border-primary/15 bg-base-200'
									: tier.id === 'personalize'
										? 'border-base-300/40 bg-base-200/70'
										: 'border-base-300/30 bg-base-200/40'}"
							>
								<div class="px-3 pb-1 pt-2">
									<h3
										class="text-xs font-bold uppercase tracking-widest {tier.id === 'setup'
											? 'text-accent'
											: tier.id === 'personalize'
												? 'text-base-content/55'
												: 'text-base-content/35'}"
									>
										{tier.label}
									</h3>
									<p class="text-[10px] text-base-content/35">{tier.hint}</p>
								</div>
								<ul class="menu gap-0.5 p-0">
									{#each tierTabs as tab (tab.id)}
										{@const Icon = tab.icon}
										{@const isActive = activeTab === tab.id}
										<li>
											<button
												class="group justify-start gap-3 rounded-xl text-base transition-all {isActive
													? 'glow-primary-soft bg-primary/15 font-semibold text-primary'
													: 'text-base-content/70 hover:bg-base-300/40'}"
												onclick={() => selectTab(tab.id)}
											>
												<Icon
													class="h-5 w-5 {isActive
														? 'text-primary'
														: 'text-base-content/50 group-hover:text-base-content/80'}"
												/>
												<span>{tab.label}</span>
												{#if tab.id in connectionMap}
													{@const storeKey = connectionMap[tab.id]}
													{@const connected = integration.current[storeKey]}
													<span
														class="ml-auto h-2 w-2 rounded-full {connected
															? 'bg-success ring-2 ring-success/30'
															: 'bg-base-content/20'}"
													>
														<span class="sr-only">{connected ? 'Connected' : 'Not connected'}</span>
													</span>
												{/if}
												{#if tab.id === 'about' && updateAvailable}
													<span
														class="ml-auto flex items-center gap-1 rounded-full bg-accent/15 px-2 py-0.5 text-xs font-semibold text-accent"
													>
														<ArrowUpCircle class="h-3 w-3" />
														Update
													</span>
												{/if}
											</button>
										</li>
									{/each}
								</ul>
							</div>
						{/if}
					{/each}

					{#if noMatches}
						<p class="px-3 py-2 text-sm text-base-content/40">No settings match "{filter}".</p>
					{/if}
				</aside>

				<main class="flex-1 min-w-0">
					{#if activeTab === 'appearance'}
						<SettingsAppearance />
					{:else if activeTab === 'settings'}
						<SettingsPreferences />
					{:else if activeTab === 'home'}
						<SettingsHome />
					{:else if activeTab === 'discover'}
						<SettingsDiscover />
					{:else if activeTab === 'music-source'}
						<SettingsMusicSource />
					{:else if activeTab === 'cache'}
						<SettingsCache />
					{:else if activeTab === 'library'}
						<SettingsLibrary />
					{:else if activeTab === 'connect-apps'}
						<SettingsConnectApps />
					{:else if activeTab === 'download-client' && authStore.isAdmin}
						<div class="space-y-6">
							<div>
								<h2 class="text-xl font-bold">Download clients</h2>
								<p class="text-sm text-base-content/60">
									Soulseek and Usenet are two ways to acquire music. Configure either or both, set
									which is tried first, and tune the shared policy.
								</p>
							</div>
							<SettingsDownloadPolicy />
							<SettingsWanted />
							<SettingsSourcePriority />
							<SettingsDownloadClient />
							<SettingsSabnzbd />
							<SettingsOnboardingChecklist />
						</div>
					{:else if activeTab === 'free-music' && authStore.isAdmin}
						<SettingsFreeMusic />
					{:else if activeTab === 'get-it' && authStore.isAdmin}
						<SettingsGetIt />
					{:else if activeTab === 'indexers' && authStore.isAdmin}
						<SettingsIndexers />
					{:else if activeTab === 'lidarr-import' && authStore.isAdmin}
						<SettingsLidarrImport />
					{:else if activeTab === 'jellyfin'}
						<SettingsJellyfin />
					{:else if activeTab === 'navidrome'}
						<SettingsNavidrome />
					{:else if activeTab === 'plex'}
						<SettingsPlex />
					{:else if activeTab === 'youtube'}
						<SettingsYouTube />
					{:else if activeTab === 'lastfm' && authStore.isAdmin}
						<SettingsLastFmApp />
					{:else if activeTab === 'spotify' && authStore.isAdmin}
						<SettingsSpotify />
					{:else if activeTab === 'events' && authStore.isAdmin}
						<SettingsEvents />
					{:else if activeTab === 'musicbrainz'}
						<SettingsMusicBrainz />
					{:else if activeTab === 'advanced'}
						<SettingsAdvanced />
					{:else if activeTab === 'about'}
						<SettingsAbout />
					{:else if activeTab === 'security' && authStore.isAdmin}
						<SettingsSecurity />
					{:else if activeTab === 'wrapped' && authStore.isAdmin}
						<SettingsWrapped />
					{:else if activeTab === 'plugins' && authStore.isAdmin}
						<SettingsPlugins />
					{:else if activeTab === 'users' && authStore.isAdmin}
						<SettingsUsers />
					{/if}
				</main>
			</div>
		{/if}
	</div>
</div>

<style>
	.dn-settings-mobilehead {
		position: sticky;
		top: calc(4rem + var(--ms-safe-top));
		z-index: 40;
		display: flex;
		align-items: center;
		gap: 0.75rem;
		margin: 0 -1rem 1rem;
		padding: 0.6rem 1rem;
		border-bottom: 1px solid var(--dn-hairline);
		background: oklch(from var(--color-base-100) l c h / 0.92);
		backdrop-filter: blur(16px);
		-webkit-backdrop-filter: blur(16px);
	}

	/* phones: settings panels breathe less — tighter cards, full-bleed feel */
	@media (max-width: 640px) {
		.dn-settings-body :global(.card-body) {
			padding: 1.1rem;
		}
		.dn-settings-body :global(.card-title) {
			font-size: 1.15rem;
		}
	}
</style>
