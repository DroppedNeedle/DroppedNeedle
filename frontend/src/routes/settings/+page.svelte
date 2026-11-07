<script lang="ts">
	import { page } from '$app/state';
	import { replaceState } from '$app/navigation';
	import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
	import SettingsTabContent from '$lib/features/settings/components/SettingsTabContent.svelte';
	import { isSettingsTabVisible } from '$lib/features/settings/components/settingsTabs';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getUpdateCheckQuery } from '$lib/shell/version.svelte';
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
		Palette,
		Activity,
		Disc3,
		DatabaseBackup
	} from 'lucide-svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import SpotifyIcon from '$lib/media/icons/SpotifyIcon.svelte';
	import YouTubeIcon from '$lib/media/icons/YouTubeIcon.svelte';
	import FritEdge from '$lib/ui/FritEdge.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import SettingsRail from '$lib/features/settings/components/SettingsRail.svelte';
	import '$lib/features/settings/components/settings.css';
	import UnsavedChangesGuard from '$lib/ui/UnsavedChangesGuard.svelte';
	import { confirmLeave } from '$lib/ui/settingsSection.svelte';

	const integration = useIntegrationStatus();

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

	const tiers = [
		{ id: 'setup', label: 'Setup', hint: 'Connect your sources' },
		{ id: 'personalize', label: 'Personalize', hint: 'Tune your content' },
		{ id: 'system', label: 'System', hint: 'Maintenance & account' }
	];

	const tabs = [
		{ id: 'library', label: 'Library', tier: 'setup', icon: Music },
		{ id: 'editions', label: 'Editions', tier: 'setup', icon: Disc3 },
		{
			id: 'download-client',
			label: 'Download Client',
			tier: 'setup',
			icon: HardDriveDownload
		},
		{ id: 'indexers', label: 'Indexers / Prowlarr', tier: 'setup', icon: Search },
		{ id: 'lidarr-import', label: 'Lidarr Import', tier: 'setup', icon: CloudDownload },
		{ id: 'connect-apps', label: 'Connect Apps', tier: 'setup', icon: Waypoints },
		{ id: 'jellyfin', label: 'Jellyfin', tier: 'setup', icon: JellyfinIcon },
		{ id: 'navidrome', label: 'Navidrome', tier: 'setup', icon: NavidromeIcon },
		{ id: 'plex', label: 'Plex', tier: 'setup', icon: PlexIcon },
		{ id: 'youtube', label: 'YouTube', tier: 'setup', icon: YouTubeIcon },
		{ id: 'lastfm', label: 'Last.fm', tier: 'setup', icon: Radio },
		{ id: 'listenbrainz', label: 'ListenBrainz', tier: 'setup', icon: AudioLines },
		{ id: 'spotify', label: 'Spotify', tier: 'setup', icon: SpotifyIcon },
		{ id: 'events', label: 'Live Events', tier: 'setup', icon: CalendarClock },
		{ id: 'get-it', label: 'Get it', tier: 'setup', icon: ShoppingBag },
		{ id: 'settings', label: 'Release Types', tier: 'personalize', icon: Settings2 },
		{ id: 'home', label: 'Home', tier: 'personalize', icon: House },
		{ id: 'discover', label: 'Discover', tier: 'personalize', icon: Compass },
		{ id: 'sidebar', label: 'Sidebar', tier: 'personalize', icon: PanelLeft },
		{ id: 'appearance', label: 'Appearance', tier: 'personalize', icon: Palette },
		{ id: 'music-source', label: 'Music Source', tier: 'personalize', icon: ChartColumn },
		{ id: 'cache', label: 'Cache', tier: 'system', icon: Database },
		{ id: 'musicbrainz', label: 'MusicBrainz', tier: 'system', icon: Globe },
		{ id: 'users', label: 'Users', tier: 'system', icon: Users },
		{ id: 'backups', label: 'Backups', tier: 'system', icon: DatabaseBackup },
		{ id: 'security', label: 'Security', tier: 'system', icon: ShieldCheck },
		{ id: 'plugins', label: 'Plugins', tier: 'system', icon: Blocks },
		{ id: 'wrapped', label: 'Wrapped API', tier: 'system', icon: Gift },
		{ id: 'diagnostics', label: 'Diagnostics', tier: 'system', icon: Activity },
		{ id: 'advanced', label: 'Advanced', tier: 'system', icon: Settings },
		{ id: 'about', label: 'About', tier: 'system', icon: Info }
	].filter((tab) => isSettingsTabVisible(tab.id, authStore.isAdmin));
	const requestedTab = page.url.searchParams.get('tab');
	let activeTab = $state(
		requestedTab && tabs.some((tab) => tab.id === requestedTab) ? requestedTab : tabs[0].id
	);

	function connected(id: string): boolean | null {
		return id in connectionMap ? integration.current[connectionMap[id]] : null;
	}

	function selectTab(id: string) {
		if (id === activeTab) return;
		confirmLeave(() => {
			activeTab = id;
			const url = new URL(page.url);
			url.searchParams.set('tab', id);
			replaceState(url, {});
		});
	}

	$effect(() => {
		const tabParam = page.url.searchParams.get('tab');
		if (tabParam && tabs.some((t) => t.id === tabParam)) {
			activeTab = tabParam;
		}
	});
</script>

<div class="dn-settings flex min-h-screen flex-col lg:h-full lg:min-h-0">
	<!-- Desktop is an app-style two-pane layout inside the shell's locked
	     viewport column: the page itself doesn't scroll; the rail and the
	     content pane each scroll on their own. Below that, the sections
	     dropdown sits over the pane in normal page flow. -->
	<div class="dn-settings__head lg:shrink-0">
		<FritEdge place="top-left" class="dn-settings__frit" />
		<FritEdge place="top-right" class="dn-settings__frit" />
		<ListPageHeader
			title="Settings"
			lede="Manage your preferences and app settings."
			ticket="DN / CC 3.2"
			compact
		/>
	</div>

	<div
		class="flex flex-col gap-6 px-4 pb-6 sm:px-6 lg:min-h-0 lg:flex-1 lg:flex-row lg:px-8 lg:pb-0"
	>
		<aside class="w-full lg:w-80 lg:shrink-0 lg:min-h-0 lg:overflow-y-auto lg:pb-6">
			<SettingsRail
				{tiers}
				{tabs}
				active={activeTab}
				onselect={selectTab}
				{connected}
				{updateAvailable}
			/>
		</aside>

		<main class="dn-set-pane min-w-0 flex-1 lg:min-h-0 lg:overflow-y-auto lg:pb-8">
			<SettingsTabContent tab={activeTab} isAdmin={authStore.isAdmin} />
		</main>
	</div>
</div>

<UnsavedChangesGuard />

<style>
	.dn-settings__head {
		position: relative;
		isolation: isolate;
	}
	.dn-settings__head :global(.dn-settings__frit) {
		z-index: -1;
	}
</style>
