<script lang="ts">
	/*
	 * AppSidebar — the audiophile-grade navigation rail.
	 *
	 * IA (mockup-derived), every real destination re-homed:
	 *   HOME · DISCOVERY · STATIONS · VINYL COLLECTION · RECENT SPUN
	 *   YOUR MUSIC: Playlists / Following / Downloads
	 *   HIGH-RES STREAMS: connected sources (+ admin connect hints)
	 *   COMMUNITY: Activity (live listening)
	 *   MANAGE: Requests / Approvals
	 * All badges, live dots, and permission gates carried over unchanged.
	 */
	import { page } from '$app/state';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { syncStatus } from '$lib/stores/syncStatus.svelte';
	import { downloadsActivity } from '$lib/stores/downloadsActivity.svelte';
	import { pendingApprovalCountStore } from '$lib/stores/pendingApprovalCountStore.svelte';
	import { nowPlayingMerged } from '$lib/stores/nowPlayingMerged.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { logout } from '$lib/utils/logout';
	import { fromStore } from 'svelte/store';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import YouTubeIcon from '$lib/components/YouTubeIcon.svelte';
	import SidebarServiceHint from '$lib/components/SidebarServiceHint.svelte';
	import SidebarVisualiser from '$lib/components/SidebarVisualiser.svelte';
	import ConcertsNavBadge from '$lib/components/ConcertsNavBadge.svelte';
	import NewReleasesNavBadge from '$lib/components/NewReleasesNavBadge.svelte';
	import {
		Search,
		House,
		Compass,
		Disc as VinylIcon,
		RadioTower,
		History,
		Download,
		Heart,
		ListMusic,
		Headphones,
		Disc3,
		Inbox,
		Activity,
		ShieldCheck,
		Settings,
		LogOut,
		PanelLeft,
		ArrowUpCircle,
		Plus
	} from 'lucide-svelte';

	let {
		versionUpdateAvailable = false,
		libraryScanActive = false
	}: { versionUpdateAvailable?: boolean; libraryScanActive?: boolean } = $props();

	const integrations = fromStore(integrationStore);

	const currentPath = $derived(page.url.pathname);
	const downloadClientConfigured = $derived(
		integrations.current.download_client || !integrations.current.loaded
	);
	const localPlaying = $derived(
		playerStore.isPlaying && playerStore.nowPlaying?.sourceType === 'local'
	);

	function isNavActive(path: string): boolean {
		return currentPath === path || currentPath.startsWith(`${path}/`);
	}

	function openSearch(): void {
		(document.getElementById('search_modal') as HTMLDialogElement)?.showModal();
	}
</script>

<div class="dn-sidebar is-drawer-close:w-[4.75rem] is-drawer-open:w-64 flex min-h-full flex-col">
	<nav class="dn-sidebar__scroll flex-1 overflow-y-auto px-3 pt-4" aria-label="Primary">
		<div class="dn-sidebar__brand">
			<span class="dn-sidebar__brandmark">
				<Disc3 class="h-5 w-5" />
			</span>
			<div class="dn-sidebar__brandtext">
				<p class="dn-sidebar__brandtitle">Library</p>
				<p class="dn-sidebar__eyebrow">Audiophile grade</p>
			</div>
		</div>

		<a href="/playlists?new=1" class="dn-sidebar__cta">
			<Plus class="h-3.5 w-3.5" />
			New playlist
		</a>

		<button type="button" class="dn-navitem w-full" data-tip="Search" onclick={openSearch}>
			<Search class="dn-navitem__icon" />
			<span class="dn-navitem__label">Search</span>
			<kbd class="dn-navitem__kbd is-drawer-close:hidden">/</kbd>
		</button>

		<ul class="dn-navlist mt-2">
			<li>
				<a
					href="/"
					class="dn-navitem"
					data-tip="Home"
					class:dn-navitem--active={currentPath === '/'}
					aria-current={currentPath === '/' ? 'page' : undefined}
				>
					<House class="dn-navitem__icon" />
					<span class="dn-navitem__label">Home</span>
				</a>
			</li>
			<li>
				<a
					href="/discover"
					class="dn-navitem"
					data-tip="Discovery"
					class:dn-navitem--active={isNavActive('/discover')}
					aria-current={isNavActive('/discover') ? 'page' : undefined}
				>
					<Compass class="dn-navitem__icon" />
					<span class="dn-navitem__label">Discovery</span>
				</a>
			</li>
			<li>
				<a
					href="/stations"
					class="dn-navitem"
					data-tip="Stations"
					class:dn-navitem--active={isNavActive('/stations')}
					aria-current={isNavActive('/stations') ? 'page' : undefined}
				>
					<RadioTower class="dn-navitem__icon" />
					<span class="dn-navitem__label">Stations</span>
				</a>
			</li>
			<li>
				<a
					href="/library"
					class="dn-navitem"
					data-tip="Vinyl Collection"
					class:dn-navitem--active={isNavActive('/library') &&
						!isNavActive('/library/jellyfin') &&
						!isNavActive('/library/navidrome') &&
						!isNavActive('/library/plex') &&
						!isNavActive('/library/youtube') &&
						!isNavActive('/library/local') &&
						!isNavActive('/library/tracks')}
					aria-current={isNavActive('/library') ? 'page' : undefined}
				>
					<span class="dn-navitem__iconwrap">
						<VinylIcon class="dn-navitem__icon" />
						{#if syncStatus.isActive || libraryScanActive}
							<span
								class="dn-dot dn-dot--primary animate-pulse"
								aria-label="Library sync in progress"
							></span>
						{/if}
					</span>
					<span class="dn-navitem__label">Vinyl Collection</span>
				</a>
			</li>
			<li>
				<a
					href="/library/tracks?sort=recent"
					class="dn-navitem"
					data-tip="Recent Spun"
					class:dn-navitem--active={isNavActive('/library/tracks')}
				>
					<History class="dn-navitem__icon" />
					<span class="dn-navitem__label">Recent Spun</span>
				</a>
			</li>
		</ul>

		<p class="dn-navgroup is-drawer-close:opacity-0">Your music</p>
		<ul class="dn-navlist">
			<li>
				<a
					href="/playlists"
					class="dn-navitem"
					data-tip="Playlists"
					class:dn-navitem--active={isNavActive('/playlists')}
					aria-current={isNavActive('/playlists') ? 'page' : undefined}
				>
					<ListMusic class="dn-navitem__icon" />
					<span class="dn-navitem__label">Playlists</span>
				</a>
			</li>
			<li>
				<a
					href="/following"
					class="dn-navitem"
					data-tip="Following"
					class:dn-navitem--active={isNavActive('/following')}
					aria-current={isNavActive('/following') ? 'page' : undefined}
				>
					<span class="dn-navitem__iconwrap">
						<Heart class="dn-navitem__icon" />
						<ConcertsNavBadge />
						<NewReleasesNavBadge />
					</span>
					<span class="dn-navitem__label">Following</span>
				</a>
			</li>
			<li>
				<a
					href="/downloads"
					class="dn-navitem"
					data-tip="Downloads"
					class:dn-navitem--active={isNavActive('/downloads')}
					aria-current={isNavActive('/downloads') ? 'page' : undefined}
				>
					<span class="dn-navitem__iconwrap">
						<Download class="dn-navitem__icon" />
						{#if downloadsActivity.isActive}
							<span
								class="dn-navitem__count animate-pulse"
								aria-label="{downloadsActivity.count} active downloads"
							>
								{downloadsActivity.count}
							</span>
						{/if}
					</span>
					<span class="dn-navitem__label">Downloads</span>
				</a>
			</li>
		</ul>

		<!-- connected streaming sources (admins see hints to connect the rest) -->
		{#if integrations.current.loaded}
			<p class="dn-navgroup is-drawer-close:opacity-0">High-res streams</p>
			<ul class="dn-navlist">
				{#if integrations.current.jellyfin}
					<li>
						<a href="/library/jellyfin" class="dn-navitem" data-tip="Jellyfin">
							<span class="dn-navitem__iconwrap">
								<JellyfinIcon class="dn-navitem__icon text-info" />
								{#if nowPlayingMerged.isSourcePlaying('jellyfin')}
									<span class="dn-dot dn-dot--primary animate-pulse"></span>
								{/if}
							</span>
							<span class="dn-navitem__label">Jellyfin</span>
							{#if nowPlayingMerged.isSourcePlaying('jellyfin')}
								<div class="now-playing-bars now-playing-bars--sm ml-auto is-drawer-close:hidden">
									<span></span><span></span><span></span>
								</div>
							{/if}
						</a>
					</li>
				{:else if authStore.isAdmin}
					<SidebarServiceHint label="Jellyfin" settingsTab="jellyfin">
						{#snippet icon()}<JellyfinIcon class="dn-navitem__icon text-info" />{/snippet}
					</SidebarServiceHint>
				{/if}

				{#if integrations.current.navidrome}
					<li>
						<a href="/library/navidrome" class="dn-navitem" data-tip="Navidrome">
							<span class="dn-navitem__iconwrap">
								<NavidromeIcon class="dn-navitem__icon text-primary" />
								{#if nowPlayingMerged.isSourcePlaying('navidrome')}
									<span class="dn-dot dn-dot--primary animate-pulse"></span>
								{/if}
							</span>
							<span class="dn-navitem__label">Navidrome</span>
							{#if nowPlayingMerged.isSourcePlaying('navidrome')}
								<div class="now-playing-bars now-playing-bars--sm ml-auto is-drawer-close:hidden">
									<span></span><span></span><span></span>
								</div>
							{/if}
						</a>
					</li>
				{:else if authStore.isAdmin}
					<SidebarServiceHint label="Navidrome" settingsTab="navidrome">
						{#snippet icon()}<NavidromeIcon class="dn-navitem__icon text-primary" />{/snippet}
					</SidebarServiceHint>
				{/if}

				{#if integrations.current.plex}
					<li>
						<a href="/library/plex" class="dn-navitem" data-tip="Plex">
							<span class="dn-navitem__iconwrap">
								<PlexIcon class="dn-navitem__icon" style="color: rgb(var(--brand-plex))" />
								{#if nowPlayingMerged.isSourcePlaying('plex')}
									<span class="dn-dot dn-dot--primary animate-pulse"></span>
								{/if}
							</span>
							<span class="dn-navitem__label">Plex</span>
							{#if nowPlayingMerged.isSourcePlaying('plex')}
								<div class="now-playing-bars now-playing-bars--sm ml-auto is-drawer-close:hidden">
									<span></span><span></span><span></span>
								</div>
							{/if}
						</a>
					</li>
				{:else if authStore.isAdmin}
					<SidebarServiceHint label="Plex" settingsTab="plex">
						{#snippet icon()}<PlexIcon
								class="dn-navitem__icon"
								style="color: rgb(var(--brand-plex))"
							/>{/snippet}
					</SidebarServiceHint>
				{/if}

				{#if integrations.current.localfiles}
					<li>
						<a href="/library/local" class="dn-navitem" data-tip="Listening Room">
							<span class="dn-navitem__iconwrap">
								<Headphones class="dn-navitem__icon text-accent" />
								{#if localPlaying}
									<span class="dn-dot" aria-label="Playing local files">
										<Disc3 class="vinyl-spin h-3 w-3 text-accent" />
									</span>
								{/if}
							</span>
							<span class="dn-navitem__label">Listening Room</span>
						</a>
					</li>
				{:else if authStore.isAdmin}
					<SidebarServiceHint label="Local Files" settingsTab="library">
						{#snippet icon()}<Headphones class="dn-navitem__icon text-accent" />{/snippet}
					</SidebarServiceHint>
				{/if}

				{#if integrations.current.youtube}
					<li>
						<a href="/library/youtube" class="dn-navitem" data-tip="YouTube">
							<YouTubeIcon class="dn-navitem__icon text-error" />
							<span class="dn-navitem__label">YouTube</span>
						</a>
					</li>
				{:else if authStore.isAdmin}
					<SidebarServiceHint label="YouTube" settingsTab="youtube">
						{#snippet icon()}<YouTubeIcon class="dn-navitem__icon text-error" />{/snippet}
					</SidebarServiceHint>
				{/if}

				{#if authStore.isAdmin}
					<SidebarVisualiser />
				{/if}
			</ul>
		{/if}

		<p class="dn-navgroup is-drawer-close:opacity-0">Community</p>
		<ul class="dn-navlist">
			<li>
				<a
					href="/activity"
					class="dn-navitem"
					data-tip="Activity"
					class:dn-navitem--active={isNavActive('/activity')}
					aria-current={isNavActive('/activity') ? 'page' : undefined}
				>
					<Activity class="dn-navitem__icon" />
					<span class="dn-navitem__label">Activity</span>
				</a>
			</li>
		</ul>

		{#if downloadClientConfigured || authStore.isAdmin}
			<p class="dn-navgroup is-drawer-close:opacity-0">Manage</p>
			<ul class="dn-navlist">
				{#if downloadClientConfigured}
					<li>
						<a
							href="/requests"
							class="dn-navitem"
							data-tip="Requests"
							class:dn-navitem--active={currentPath === '/requests' &&
								page.url.searchParams.get('tab') !== 'approvals'}
						>
							<Inbox class="dn-navitem__icon" />
							<span class="dn-navitem__label">Requests</span>
						</a>
					</li>
				{/if}
				{#if authStore.isAdmin}
					<li>
						<a href="/requests?tab=approvals" class="dn-navitem" data-tip="Approvals">
							<span class="dn-navitem__iconwrap">
								<ShieldCheck class="dn-navitem__icon" />
								{#if pendingApprovalCountStore.count > 0}
									<span class="dn-navitem__count dn-navitem__count--warning">
										{pendingApprovalCountStore.count}
									</span>
								{/if}
							</span>
							<span class="dn-navitem__label">Approvals</span>
						</a>
					</li>
				{/if}
			</ul>
		{/if}
	</nav>

	<!-- pinned footer -->
	<div class="dn-sidebar__footer" class:pb-24={playerStore.isPlayerVisible}>
		{#if authStore.isAdmin}
			<a
				href={versionUpdateAvailable ? '/settings?tab=about' : '/settings'}
				class="dn-navitem"
				data-tip={versionUpdateAvailable ? 'Settings — update available' : 'Settings'}
				class:dn-navitem--active={isNavActive('/settings')}
				aria-current={isNavActive('/settings') ? 'page' : undefined}
			>
				<span class="dn-navitem__iconwrap">
					<Settings class="dn-navitem__icon" />
					{#if versionUpdateAvailable}
						<span class="dn-navitem__badge">
							<ArrowUpCircle class="h-3 w-3" />
						</span>
					{/if}
				</span>
				<span class="dn-navitem__label">Settings</span>
			</a>
		{/if}
		<button type="button" class="dn-navitem" data-tip="Log out" onclick={() => void logout()}>
			<LogOut class="dn-navitem__icon" />
			<span class="dn-navitem__label">Log out</span>
		</button>
		<label
			for="main-drawer"
			class="dn-navitem dn-navitem--toggle drawer-button cursor-pointer max-md:hidden"
			data-tip="Collapse"
		>
			<PanelLeft class="dn-navitem__icon is-drawer-open:rotate-y-180" />
			<span class="dn-navitem__label">Collapse</span>
		</label>
	</div>
</div>
