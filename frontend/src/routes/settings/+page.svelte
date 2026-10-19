<script lang="ts">
	import { page } from '$app/state';
	import { replaceState } from '$app/navigation';
	import { onMount } from 'svelte';
	import { fromStore } from 'svelte/store';
	import { integrationStore } from '$lib/stores/integration';
	import SettingsTabContent from '$lib/components/settings/SettingsTabContent.svelte';
	import { isSettingsTabVisible } from '$lib/components/settings/settingsTabs';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getUpdateCheckQuery } from '$lib/queries/VersionQuery.svelte';
	import {
		Settings2,
		Music,
		Database,
		Settings,
		Radio,
		AudioLines,
		Search,
		ChartColumn,
		Info,
		CircleArrowUp,
		Globe,
		House,
		Compass,
		Users,
		ShieldCheck,
		HardDriveDownload,
		Waypoints,
		CalendarClock,
		CloudDownload,
		Gift,
		ShoppingBag,
		Blocks,
		PanelLeft,
		Activity,
		Disc3
	} from 'lucide-svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import SpotifyIcon from '$lib/components/SpotifyIcon.svelte';
	import YouTubeIcon from '$lib/components/YouTubeIcon.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';
	import UnsavedChangesGuard from '$lib/ui/UnsavedChangesGuard.svelte';
	import { confirmLeave } from '$lib/ui/settingsSection.svelte';

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

	let filter = $state('');

	const tiers = [
		{ id: 'setup', label: 'Setup', hint: 'Connect your sources' },
		{ id: 'personalize', label: 'Personalize', hint: 'Tune your content' },
		{ id: 'system', label: 'System', hint: 'Maintenance & account' }
	];

	const tabs = [
		{ id: 'library', label: 'Library', tier: 'setup', icon: Music },
		...(authStore.isAdmin
			? [{ id: 'editions', label: 'Editions', tier: 'setup', icon: Disc3 }]
			: []),
		...(authStore.isAdmin
			? [
					{
						id: 'download-client',
						label: 'Download Client',
						tier: 'setup',
						icon: HardDriveDownload
					},
					{ id: 'indexers', label: 'Indexers / Prowlarr', tier: 'setup', icon: Search },
					{ id: 'lidarr-import', label: 'Lidarr Import', tier: 'setup', icon: CloudDownload }
				]
			: []),
		{ id: 'connect-apps', label: 'Connect Apps', tier: 'setup', icon: Waypoints },
		{ id: 'jellyfin', label: 'Jellyfin', tier: 'setup', icon: JellyfinIcon },
		{ id: 'navidrome', label: 'Navidrome', tier: 'setup', icon: NavidromeIcon },
		{ id: 'plex', label: 'Plex', tier: 'setup', icon: PlexIcon },
		{ id: 'youtube', label: 'YouTube', tier: 'setup', icon: YouTubeIcon },
		...(authStore.isAdmin ? [{ id: 'lastfm', label: 'Last.fm', tier: 'setup', icon: Radio }] : []),
		...(authStore.isAdmin
			? [{ id: 'listenbrainz', label: 'ListenBrainz', tier: 'setup', icon: AudioLines }]
			: []),
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
		{ id: 'home', label: 'Home', tier: 'personalize', icon: House },
		{ id: 'discover', label: 'Discover', tier: 'personalize', icon: Compass },
		{ id: 'sidebar', label: 'Sidebar', tier: 'personalize', icon: PanelLeft },
		{ id: 'music-source', label: 'Music Source', tier: 'personalize', icon: ChartColumn },
		{ id: 'cache', label: 'Cache', tier: 'system', icon: Database },
		...(isSettingsTabVisible('musicbrainz', authStore.isAdmin)
			? [{ id: 'musicbrainz', label: 'MusicBrainz', tier: 'system', icon: Globe }]
			: []),
		...(authStore.isAdmin
			? [
					{ id: 'users', label: 'Users', tier: 'system', icon: Users },
					{ id: 'security', label: 'Security', tier: 'system', icon: ShieldCheck },
					{ id: 'plugins', label: 'Plugins', tier: 'system', icon: Blocks },
					{ id: 'wrapped', label: 'Wrapped API', tier: 'system', icon: Gift },
					{ id: 'diagnostics', label: 'Diagnostics', tier: 'system', icon: Activity }
				]
			: []),
		{ id: 'advanced', label: 'Advanced', tier: 'system', icon: Settings },
		{ id: 'about', label: 'About', tier: 'system', icon: Info }
	];
	const requestedTab = page.url.searchParams.get('tab');
	let activeTab = $state(
		requestedTab && tabs.some((tab) => tab.id === requestedTab) ? requestedTab : 'library'
	);

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
		if (id === activeTab) return;
		confirmLeave(() => {
			activeTab = id;
			const url = new URL(page.url);
			url.searchParams.set('tab', id);
			replaceState(url, {});
		});
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

<div class="flex min-h-screen flex-col bg-base-100 lg:h-full lg:min-h-0">
	<!-- Desktop is an app-style two-pane layout inside the shell's locked
	     viewport column: the page itself doesn't scroll; the tab rail and the
	     content pane each scroll independently, so the wheel isn't trapped by a
	     hidden sidebar scroller when the tab list grows taller than the
	     viewport. Mobile keeps natural page flow. -->
	<div class="container mx-auto p-4 max-w-7xl lg:flex lg:h-full lg:min-h-0 lg:flex-col">
		<div class="mb-6 lg:shrink-0">
			<h1 class="text-3xl font-bold">Settings</h1>
			<p class="text-base-content/70 mt-2">Manage your preferences and app settings.</p>
		</div>

		<div class="flex flex-col lg:flex-row gap-6 lg:min-h-0 lg:flex-1">
			<aside class="w-full lg:w-80 lg:shrink-0 space-y-3 lg:min-h-0 lg:overflow-y-auto lg:pb-4">
				<Input
					size="sm"
					icon={Search}
					label="Filter settings"
					placeholder="Filter settings…"
					class="w-full"
					bind:value={filter}
				/>

				{#each tiers as tier (tier.id)}
					{@const tierTabs = tabsForTier(tier.id)}
					{#if tierTabs.length > 0}
						<Card variant="bordered" padding="sm">
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
											aria-current={isActive ? 'page' : undefined}
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
													<CircleArrowUp class="h-3 w-3" />
													Update
												</span>
											{/if}
										</button>
									</li>
								{/each}
							</ul>
						</Card>
					{/if}
				{/each}

				{#if noMatches}
					<p class="px-3 py-2 text-sm text-base-content/40">No settings match "{filter}".</p>
				{/if}
			</aside>

			<main class="flex-1 min-w-0 lg:min-h-0 lg:overflow-y-auto lg:pb-4">
				<SettingsTabContent tab={activeTab} isAdmin={authStore.isAdmin} />
			</main>
		</div>
	</div>
</div>

<UnsavedChangesGuard />
