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
	import SettingsSidebar from '$lib/components/settings/SettingsSidebar.svelte';
	import SettingsUsers from '$lib/components/settings/SettingsUsers.svelte';
	import SettingsSecurity from '$lib/components/settings/SettingsSecurity.svelte';
	import SettingsDownloadClient from '$lib/components/settings/SettingsDownloadClient.svelte';
	import SettingsProwlarr from '$lib/components/settings/SettingsProwlarr.svelte';
	import SettingsQbittorrent from '$lib/components/settings/SettingsQbittorrent.svelte';
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
		PanelLeft
	} from 'lucide-svelte';
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

	let activeTab = $state('library');
	let filter = $state('');

	const tiers = [
		{ id: 'setup', label: 'Setup', hint: 'Connect your sources' },
		{ id: 'personalize', label: 'Personalize', hint: 'Tune your content' },
		{ id: 'system', label: 'System', hint: 'Maintenance & account' }
	];

	const tabs = [
		{ id: 'library', label: 'Library', tier: 'setup', icon: Music },
		...(authStore.isAdmin
			? [
					{ id: 'free-music', label: 'Free Music', tier: 'setup', icon: Landmark },
					{
						id: 'download-client',
						label: 'Download Client',
						tier: 'setup',
						icon: HardDriveDownload
					},
					{ id: 'indexers', label: 'Indexers', tier: 'setup', icon: Search },
					{ id: 'lidarr-import', label: 'Lidarr Import', tier: 'setup', icon: DownloadCloud }
				]
			: []),
		{ id: 'connect-apps', label: 'Connect Apps', tier: 'setup', icon: Waypoints },
		{ id: 'jellyfin', label: 'Jellyfin', tier: 'setup', icon: JellyfinIcon },
		{ id: 'navidrome', label: 'Navidrome', tier: 'setup', icon: NavidromeIcon },
		{ id: 'plex', label: 'Plex', tier: 'setup', icon: PlexIcon },
		{ id: 'youtube', label: 'YouTube', tier: 'setup', icon: Youtube },
		...(authStore.isAdmin ? [{ id: 'lastfm', label: 'Last.fm', tier: 'setup', icon: Radio }] : []),
		...(authStore.isAdmin
			? [{ id: 'spotify', label: 'Spotify', tier: 'setup', icon: SpotifyIcon }]
			: []),
		...(authStore.isAdmin
			? [{ id: 'events', label: 'Live Events', tier: 'setup', icon: CalendarClock }]
			: []),
		...(authStore.isAdmin
			? [{ id: 'get-it', label: 'Get it', tier: 'setup', icon: ShoppingBag }]
			: []),
		{ id: 'settings', label: 'Release Types', tier: 'personalize', icon: Settings2 },
		{ id: 'home', label: 'Home', tier: 'personalize', icon: Home },
		{ id: 'discover', label: 'Discover', tier: 'personalize', icon: Compass },
		{ id: 'sidebar', label: 'Sidebar', tier: 'personalize', icon: PanelLeft },
		{ id: 'music-source', label: 'Music Source', tier: 'personalize', icon: BarChart3 },
		{ id: 'cache', label: 'Cache', tier: 'system', icon: Database },
		{ id: 'musicbrainz', label: 'MusicBrainz', tier: 'system', icon: Globe },
		...(authStore.isAdmin
			? [
					{ id: 'users', label: 'Users', tier: 'system', icon: Users },
					{ id: 'security', label: 'Security', tier: 'system', icon: ShieldCheck },
					{ id: 'plugins', label: 'Plugins', tier: 'system', icon: Blocks },
					{ id: 'wrapped', label: 'Wrapped API', tier: 'system', icon: Gift }
				]
			: []),
		{ id: 'advanced', label: 'Advanced', tier: 'system', icon: Settings },
		{ id: 'about', label: 'About', tier: 'system', icon: Info }
	];

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
		const url = new URL(page.url);
		url.searchParams.set('tab', id);
		replaceState(url, {});
	}

	onMount(() => {
		integrationStore.ensureLoaded();
	});

	$effect(() => {
		const tabParam = page.url.searchParams.get('tab');
		if (tabParam && tabs.some((t) => t.id === tabParam)) {
			activeTab = tabParam;
		}
	});
</script>

<div class="min-h-screen bg-base-100">
	<!-- Desktop is an app-style two-pane layout: the page itself doesn't scroll;
	     the tab rail and the content pane each scroll independently. This is what
	     keeps the wheel from being trapped by a hidden sidebar scroller when the
	     tab list grows taller than the viewport. Mobile keeps natural page flow. -->
	<div class="container mx-auto p-4 max-w-7xl lg:flex lg:h-[calc(100vh-4rem)] lg:flex-col">
		<div class="mb-6 lg:shrink-0">
			<h1 class="text-3xl font-bold">Settings</h1>
			<p class="text-base-content/70 mt-2">Manage your preferences and app settings.</p>
		</div>

		<div class="flex flex-col lg:flex-row gap-6 lg:min-h-0 lg:flex-1">
			<aside
				class="scrollbar-hide w-full lg:w-80 lg:shrink-0 space-y-3 lg:min-h-0 lg:overflow-y-auto lg:pb-4"
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

			<main class="flex-1 min-w-0 lg:min-h-0 lg:overflow-y-auto lg:pb-4">
				{#if activeTab === 'settings'}
					<SettingsPreferences />
				{:else if activeTab === 'home'}
					<SettingsHome />
				{:else if activeTab === 'discover'}
					<SettingsDiscover />
				{:else if activeTab === 'sidebar'}
					<SettingsSidebar />
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
						<SettingsProwlarr />
						<SettingsQbittorrent />
						<SettingsOnboardingChecklist />
					</div>
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
				{:else if activeTab === 'free-music' && authStore.isAdmin}
					<SettingsFreeMusic />
				{:else if activeTab === 'get-it' && authStore.isAdmin}
					<SettingsGetIt />
				{:else if activeTab === 'plugins' && authStore.isAdmin}
					<SettingsPlugins />
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
				{:else if activeTab === 'users' && authStore.isAdmin}
					<SettingsUsers />
				{/if}
			</main>
		</div>
	</div>
</div>
