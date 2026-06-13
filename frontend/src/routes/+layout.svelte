<script lang="ts">
	import '../app.css';
	import { browser } from '$app/environment';
	import { goto, beforeNavigate, afterNavigate } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { AUTH_FREE_PATHS } from '$lib/constants';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { migratePageSourceKeys } from '$lib/stores/musicSource';
	import { errorModal } from '$lib/stores/errorModal';
	import { libraryStore } from '$lib/stores/library';
	import { integrationStore } from '$lib/stores/integration';
	import { initCacheTTLs } from '$lib/stores/cacheTtl';
	import { playerStore } from '$lib/stores/player.svelte';
	import { launchYouTubePlayback } from '$lib/player/launchYouTubePlayback';
	import { playbackToast } from '$lib/stores/playbackToast.svelte';
	import { scrobbleManager } from '$lib/stores/scrobble.svelte';
	import { imageSettingsStore } from '$lib/stores/imageSettings';
	import { serviceStatusStore } from '$lib/stores/serviceStatus';
	import { resumeAudioEngine, setAudioElement } from '$lib/player/audioElement';
	import { eqStore } from '$lib/stores/eq.svelte';
	import Player from '$lib/components/Player.svelte';
	import CacheSyncIndicator from '$lib/components/CacheSyncIndicator.svelte';
	import AddToPlaylistModal, {
		registerPlaylistModal,
		unregisterPlaylistModal
	} from '$lib/components/AddToPlaylistModal.svelte';
	import DiscographyDownloadModal from '$lib/components/DiscographyDownloadModal.svelte';
	import BatchDownloadIndicator from '$lib/components/BatchDownloadIndicator.svelte';
	import { syncStatus } from '$lib/stores/syncStatus.svelte';
	import YouTubeIcon from '$lib/components/YouTubeIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import SidebarServiceHint from '$lib/components/SidebarServiceHint.svelte';
	import DegradedBanner from '$lib/components/DegradedBanner.svelte';
	import VersionOverlays from '$lib/components/VersionOverlays.svelte';
	import SearchSuggestions from '$lib/components/SearchSuggestions.svelte';
	import type { SuggestResult } from '$lib/types';
	import { onMount, onDestroy } from 'svelte';
	import { cancelPendingImages } from '$lib/utils/lazyImage';
	import { abortAllPageRequests } from '$lib/utils/navigationAbort';
	import { requestCountStore } from '$lib/stores/requestCountStore.svelte';
	import { pendingApprovalCountStore } from '$lib/stores/pendingApprovalCountStore.svelte';
	import { nowPlayingMerged } from '$lib/stores/nowPlayingMerged.svelte';
	import { nowPlayingStore } from '$lib/stores/nowPlayingSessions.svelte';
	import SidebarVisualiser from '$lib/components/SidebarVisualiser.svelte';
	import { createNavigationProgressController } from '$lib/utils/navigationProgress';
	import { fromStore } from 'svelte/store';
	import {
		Settings,
		Search,
		House,
		Compass,
		Menu,
		Headphones,
		Download,
		PanelLeft,
		TriangleAlert,
		Info,
		X,
		UserRound,
		ListMusic,
		ArrowUpCircle,
		LogOut,
		ShieldCheck
	} from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import QueryProvider from '$lib/queries/QueryProvider.svelte';

	migratePageSourceKeys();

	let { children }: { children: Snippet } = $props();

	let query = $state('');
	let audioElement = $state<HTMLAudioElement | undefined>(undefined);
	let playlistModalRef: AddToPlaylistModal | undefined = $state(undefined);
	let modalQuery = $state('');
	let showNavigationProgress = $state(false);
	let currentPath = $state('/');
	let versionUpdateAvailable = $state(false);

	const NAV_PROGRESS_DELAY_MS = 120;
	const NAV_PROGRESS_MIN_VISIBLE_MS = 220;
	const navigationProgress = createNavigationProgressController({
		delayMs: NAV_PROGRESS_DELAY_MS,
		minVisibleMs: NAV_PROGRESS_MIN_VISIBLE_MS,
		onVisibleChange: (visible) => {
			showNavigationProgress = visible;
		}
	});

	beforeNavigate((navigation) => {
		const fromPath = navigation.from?.url.pathname;
		const toPath = navigation.to?.url.pathname;
		if (fromPath !== toPath) {
			abortAllPageRequests();
			serviceStatusStore.clear();
		}
		navigationProgress.start();
		cancelPendingImages();
	});

	afterNavigate(() => {
		if (browser) {
			currentPath = window.location.pathname;
		}
		navigationProgress.finish();
		libraryStore.refreshIfStale(10_000);
	});

	let cleanupResumeListeners: (() => void) | null = null;

	onMount(() => {
		if (audioElement) {
			setAudioElement(audioElement);
			eqStore.replayToEngine();
		}

		const resumeAudioContext = () => {
			void resumeAudioEngine();
			cleanupResumeListeners?.();
			cleanupResumeListeners = null;
		};
		document.addEventListener('click', resumeAudioContext, { once: true });
		document.addEventListener('keydown', resumeAudioContext, { once: true });
		cleanupResumeListeners = () => {
			document.removeEventListener('click', resumeAudioContext);
			document.removeEventListener('keydown', resumeAudioContext);
		};

		if (browser) {
			currentPath = window.location.pathname;
		}
		initCacheTTLs();
		document.addEventListener('keydown', handleGlobalKeydown);
		if (playlistModalRef) registerPlaylistModal(playlistModalRef);

		const deferInit = (fn: () => void) => {
			if ('requestIdleCallback' in window) {
				requestIdleCallback(fn, { timeout: 2000 });
			} else {
				setTimeout(fn, 100);
			}
		};
		deferInit(() => {
			libraryStore.initialize();
			void imageSettingsStore.load();
			void restorePlayerSession();
			void scrobbleManager.init();
			requestCountStore.startPolling();
			if (authStore.isAdmin) pendingApprovalCountStore.startPolling();
			syncStatus.connect();
		});
		integrationStore.ensureLoaded().then(() => {
			nowPlayingStore.start();
		});
	});

	onDestroy(() => {
		navigationProgress.cleanup();
		cleanupResumeListeners?.();
		cleanupResumeListeners = null;
		if (browser) {
			document.removeEventListener('keydown', handleGlobalKeydown);
		}
		requestCountStore.stopPolling();
		pendingApprovalCountStore.stopPolling();
		syncStatus.disconnect();
		nowPlayingStore.stop();
		unregisterPlaylistModal();
	});

	function handleGlobalKeydown(e: KeyboardEvent): void {
		const tag = (e.target as HTMLElement)?.tagName;
		if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
		if (!playerStore.isPlayerVisible) return;

		switch (e.key) {
			case ' ':
				e.preventDefault();
				playerStore.togglePlay();
				break;
			case 'ArrowRight':
				e.preventDefault();
				playerStore.seekTo(Math.min(playerStore.progress + 10, playerStore.duration));
				break;
			case 'ArrowLeft':
				e.preventDefault();
				playerStore.seekTo(Math.max(playerStore.progress - 10, 0));
				break;
			case 'ArrowUp':
				e.preventDefault();
				playerStore.setVolume(playerStore.volume + 5);
				break;
			case 'ArrowDown':
				e.preventDefault();
				playerStore.setVolume(playerStore.volume - 5);
				break;
		}
	}

	async function restorePlayerSession(): Promise<void> {
		const session = playerStore.restoreSession();
		if (!session) return;

		try {
			if (session.nowPlaying.sourceType === 'youtube') {
				if (!session.nowPlaying.trackSourceId) return;
				await launchYouTubePlayback({
					albumId: session.nowPlaying.albumId,
					albumName: session.nowPlaying.albumName,
					artistName: session.nowPlaying.artistName,
					coverUrl: session.nowPlaying.coverUrl,
					videoId: session.nowPlaying.trackSourceId,
					embedUrl: session.nowPlaying.embedUrl
				});
			} else {
				playerStore.resumeSession();
			}
		} catch {
			return;
		}
	}

	function handleSearch() {
		if (query.trim()) {
			goto(`/search?q=${encodeURIComponent(query)}`);
		}
	}

	function handleModalSearch() {
		if (modalQuery.trim()) {
			goto(`/search?q=${encodeURIComponent(modalQuery)}`);
			const modal = document.getElementById('search_modal') as HTMLDialogElement;
			if (modal) modal.close();
			modalQuery = '';
		}
	}

	function handleSuggestionSelect(result: SuggestResult) {
		const routeId = result.type === 'artist' ? '/artist/[id]' : '/album/[id]';
		goto(resolve(routeId, { id: result.musicbrainz_id }));
	}

	function handleModalSuggestionSelect(result: SuggestResult) {
		(document.getElementById('search_modal') as HTMLDialogElement)?.close();
		const routeId = result.type === 'artist' ? '/artist/[id]' : '/album/[id]';
		goto(resolve(routeId, { id: result.musicbrainz_id }));
	}

	function isNavActive(path: string): boolean {
		return currentPath === path || currentPath.startsWith(`${path}/`);
	}

	const integrations = fromStore(integrationStore);
	const lidarrConfigured = $derived(integrations.current.lidarr || !integrations.current.loaded);
	const showAppShell = $derived(!AUTH_FREE_PATHS.some((p) => page.url.pathname.startsWith(p)));

	async function handleLogout() {
		try {
			await fetch('/api/v1/auth/logout', { method: 'POST', credentials: 'include' });
		} catch {
			// ignore
		}
		authStore.clear();
		goto('/login');
	}
</script>

<QueryProvider>
	<div data-theme="musicseerr" class="musicseerr-app-shell">
		{#if showNavigationProgress}
			<div class="fixed top-0 left-0 right-0 z-120 pointer-events-none">
				<progress class="progress progress-primary w-full h-1"></progress>
			</div>
		{/if}

		{#if showAppShell}
			<DegradedBanner />
			<VersionOverlays bind:updateAvailable={versionUpdateAvailable} />

			<div class="drawer lg:drawer-open">
				<input id="main-drawer" type="checkbox" class="drawer-toggle" />

				<div class="drawer-content flex min-w-0 flex-col">
					<div
						class="musicseerr-topbar navbar bg-base-100/95 backdrop-blur shadow-sm sticky top-0 z-50"
					>
						<div class="navbar-start w-auto">
							<a href="/" class="btn btn-ghost px-2 max-xs:hidden sm:px-4" aria-label="Home">
								<img src="/logo_wide.png" alt="Musicseerr" class="h-8 hidden sm:block" />
								<img src="/logo_icon.png" alt="Musicseerr" class="h-8 block sm:hidden" />
							</a>
						</div>
						<div class="navbar-center min-w-0 grow justify-center px-1 sm:px-4">
							<div class="w-full max-w-2xl">
								<SearchSuggestions
									bind:query
									onSearch={handleSearch}
									onSelect={handleSuggestionSelect}
									id="navbar-suggest"
								/>
							</div>
						</div>
						<div class="navbar-end w-auto pr-1 sm:pr-2">
							<a href="/profile" class="btn btn-ghost btn-circle btn-md" aria-label="Profile">
								<UserRound class="h-6 w-6" />
							</a>
						</div>
					</div>

					<div
						class="musicseerr-main-content flex-1"
						class:musicseerr-player-visible={playerStore.isPlayerVisible}
					>
						{@render children()}
					</div>
				</div>

				<div class="drawer-side hidden lg:block is-drawer-close:overflow-visible">
					<label for="main-drawer" aria-label="close sidebar" class="drawer-overlay"></label>
					<div
						class="is-drawer-close:w-16 is-drawer-open:w-64 bg-base-200 flex flex-col items-start min-h-full"
					>
						<ul class="menu w-full grow p-2 [&_li>*]:py-3">
							<li>
								<button
									onclick={() =>
										(document.getElementById('search_modal') as HTMLDialogElement)?.showModal()}
									class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
									data-tip="Search"
								>
									<Search class="h-6 w-6" />
									<span class="is-drawer-close:hidden">Search</span>
								</button>
							</li>

							<div class="divider my-0"></div>

							<li>
								<a
									href="/"
									class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
									data-tip="Home"
								>
									<House class="h-6 w-6" />
									<span class="is-drawer-close:hidden">Home</span>
								</a>
							</li>

							<li>
								<a
									href="/discover"
									class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
									data-tip="Discover"
								>
									<Compass class="h-6 w-6" />
									<span class="is-drawer-close:hidden">Discover</span>
								</a>
							</li>

							{#if lidarrConfigured}
								<li>
									<a
										href="/library"
										class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
										data-tip="Library"
									>
										<div class="relative">
											<Menu class="h-6 w-6" />
											{#if syncStatus.isActive}
												<span
													class="absolute -top-1 -right-1 badge badge-primary badge-xs w-2.5 h-2.5 p-0 animate-pulse"
													aria-label="Library sync in progress"
												></span>
											{/if}
										</div>
										<span class="is-drawer-close:hidden">Library</span>
									</a>
								</li>

								<li>
									<a
										href="/playlists"
										class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
										class:menu-active={isNavActive('/playlists')}
										aria-current={isNavActive('/playlists') ? 'page' : undefined}
										data-tip="Playlists"
									>
										<ListMusic class="h-6 w-6" />
										<span class="is-drawer-close:hidden">Playlists</span>
									</a>
								</li>
							{/if}

							{#if integrations.current.loaded}
								<div class="divider my-0"></div>
							{/if}

							{#if integrations.current.youtube}
								<li>
									<a
										href="/library/youtube"
										class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
										data-tip="YouTube"
									>
										<YouTubeIcon class="h-6 w-6 text-error" />
										<span class="is-drawer-close:hidden">YouTube</span>
									</a>
								</li>
							{:else if integrations.current.loaded && authStore.isAdmin}
								<SidebarServiceHint label="YouTube" settingsTab="youtube">
									{#snippet icon()}<YouTubeIcon class="h-6 w-6 text-error" />{/snippet}
								</SidebarServiceHint>
							{/if}

							{#if integrations.current.jellyfin}
								<li>
									<a
										href="/library/jellyfin"
										class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
										data-tip="Jellyfin"
									>
										<div class="relative inline-flex">
											<JellyfinIcon class="h-6 w-6 text-info" />
											{#if nowPlayingMerged.isSourcePlaying('jellyfin')}
												<span
													class="absolute -top-0.5 -right-0.5 h-2 w-2 rounded-full bg-primary animate-pulse"
												></span>
											{/if}
										</div>
										<span class="is-drawer-close:hidden">Jellyfin</span>
										{#if nowPlayingMerged.isSourcePlaying('jellyfin')}
											<div
												class="now-playing-bars now-playing-bars--sm ml-auto is-drawer-close:hidden"
											>
												<span></span><span></span><span></span>
											</div>
										{/if}
									</a>
								</li>
							{:else if integrations.current.loaded && authStore.isAdmin}
								<SidebarServiceHint label="Jellyfin" settingsTab="jellyfin">
									{#snippet icon()}<JellyfinIcon class="h-6 w-6 text-info" />{/snippet}
								</SidebarServiceHint>
							{/if}

							{#if integrations.current.navidrome}
								<li>
									<a
										href="/library/navidrome"
										class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
										data-tip="Navidrome"
									>
										<div class="relative inline-flex">
											<NavidromeIcon class="h-6 w-6 text-primary" />
											{#if nowPlayingMerged.isSourcePlaying('navidrome')}
												<span
													class="absolute -top-0.5 -right-0.5 h-2 w-2 rounded-full bg-primary animate-pulse"
												></span>
											{/if}
										</div>
										<span class="is-drawer-close:hidden">Navidrome</span>
										{#if nowPlayingMerged.isSourcePlaying('navidrome')}
											<div
												class="now-playing-bars now-playing-bars--sm ml-auto is-drawer-close:hidden"
											>
												<span></span><span></span><span></span>
											</div>
										{/if}
									</a>
								</li>
							{:else if integrations.current.loaded && authStore.isAdmin}
								<SidebarServiceHint label="Navidrome" settingsTab="navidrome">
									{#snippet icon()}<NavidromeIcon class="h-6 w-6 text-primary" />{/snippet}
								</SidebarServiceHint>
							{/if}

							{#if integrations.current.plex}
								<li>
									<a
										href="/library/plex"
										class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
										data-tip="Plex"
									>
										<div class="relative inline-flex">
											<PlexIcon class="h-6 w-6" style="color: rgb(var(--brand-plex))" />
											{#if nowPlayingMerged.isSourcePlaying('plex')}
												<span
													class="absolute -top-0.5 -right-0.5 h-2 w-2 rounded-full bg-primary animate-pulse"
												></span>
											{/if}
										</div>
										<span class="is-drawer-close:hidden">Plex</span>
										{#if nowPlayingMerged.isSourcePlaying('plex')}
											<div
												class="now-playing-bars now-playing-bars--sm ml-auto is-drawer-close:hidden"
											>
												<span></span><span></span><span></span>
											</div>
										{/if}
									</a>
								</li>
							{:else if integrations.current.loaded && authStore.isAdmin}
								<SidebarServiceHint label="Plex" settingsTab="plex">
									{#snippet icon()}<PlexIcon
											class="h-6 w-6"
											style="color: rgb(var(--brand-plex))"
										/>{/snippet}
								</SidebarServiceHint>
							{/if}

							{#if integrations.current.localfiles}
								<li>
									<a
										href="/library/local"
										class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
										data-tip="Local Files"
									>
										<Headphones class="h-6 w-6 text-accent" />
										<span class="is-drawer-close:hidden">Local Files</span>
									</a>
								</li>
							{:else if integrations.current.loaded && authStore.isAdmin}
								<SidebarServiceHint label="Local Files" settingsTab="local-files">
									{#snippet icon()}<Headphones class="h-6 w-6 text-accent" />{/snippet}
								</SidebarServiceHint>
							{/if}

							{#if authStore.isAdmin}
								<SidebarVisualiser />
							{/if}

							{#if lidarrConfigured}
								<div class="divider my-0"></div>
								<li>
									<a
										href="/requests"
										class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
										data-tip="Requests"
									>
										<div class="relative">
											<Download class="h-6 w-6" />
											{#if requestCountStore.count > 0}
												<span
													class="absolute -top-2 -right-2 badge badge-info badge-xs w-4 h-4 p-0 text-[10px] font-bold"
													>{requestCountStore.count}</span
												>
											{/if}
										</div>
										<span class="is-drawer-close:hidden">Requests</span>
									</a>
								</li>
							{/if}

							{#if authStore.isAdmin}
								<li>
									<a
										href="/requests?tab=approvals"
										class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
										data-tip="Approvals"
									>
										<div class="relative">
											<ShieldCheck class="h-6 w-6" />
											{#if pendingApprovalCountStore.count > 0}
												<span
													class="absolute -top-2 -right-2 badge badge-warning badge-xs w-4 h-4 p-0 text-[10px] font-bold"
													>{pendingApprovalCountStore.count}</span
												>
											{/if}
										</div>
										<span class="is-drawer-close:hidden">Approvals</span>
									</a>
								</li>
							{/if}
						</ul>
						<div class="w-full p-2 flex flex-col gap-1" class:pb-24={playerStore.isPlayerVisible}>
							{#if authStore.isAdmin}
								<div
									class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
									data-tip={versionUpdateAvailable ? 'Settings - update available' : 'Settings'}
								>
									<a
										href={versionUpdateAvailable ? '/settings?tab=about' : '/settings'}
										class="btn btn-ghost btn-circle relative"
										aria-label={versionUpdateAvailable ? 'Settings - update available' : 'Settings'}
									>
										<Settings class="h-6 w-6" />
										{#if versionUpdateAvailable}
											<span
												class="absolute -top-0.5 -right-0.5 flex h-4.5 w-4.5 items-center justify-center rounded-full bg-accent text-accent-content shadow-sm shadow-accent/30"
											>
												<ArrowUpCircle class="h-3 w-3" />
											</span>
										{/if}
									</a>
								</div>
							{/if}
							<div class="is-drawer-close:tooltip is-drawer-close:tooltip-right" data-tip="Log out">
								<button
									onclick={() => void handleLogout()}
									class="btn btn-ghost btn-circle"
									aria-label="Log out"
								>
									<LogOut class="h-6 w-6" />
								</button>
							</div>
							<div class="is-drawer-close:tooltip is-drawer-close:tooltip-right" data-tip="Open">
								<label
									for="main-drawer"
									class="btn btn-ghost btn-circle drawer-button is-drawer-open:rotate-y-180"
								>
									<PanelLeft class="h-6 w-6" />
								</label>
							</div>
						</div>
					</div>
				</div>
			</div>
		{:else}
			{@render children()}
		{/if}

		<nav class="musicseerr-bottom-nav" aria-label="Primary navigation">
			<a
				href="/"
				class="musicseerr-bottom-nav__item"
				class:active={currentPath === '/'}
				aria-current={currentPath === '/' ? 'page' : undefined}
			>
				<House />
				<span>Home</span>
			</a>
			<a
				href="/discover"
				class="musicseerr-bottom-nav__item"
				class:active={isNavActive('/discover')}
				aria-current={isNavActive('/discover') ? 'page' : undefined}
			>
				<Compass />
				<span>Discover</span>
			</a>
			<button
				type="button"
				class="musicseerr-bottom-nav__item"
				class:active={isNavActive('/search')}
				onclick={() => (document.getElementById('search_modal') as HTMLDialogElement)?.showModal()}
				aria-current={isNavActive('/search') ? 'page' : undefined}
			>
				<Search />
				<span>Search</span>
			</button>
			<a
				href="/library"
				class="musicseerr-bottom-nav__item"
				class:active={isNavActive('/library')}
				aria-current={isNavActive('/library') ? 'page' : undefined}
			>
				<Menu />
				<span>Library</span>
				{#if syncStatus.isActive}
					<span class="musicseerr-bottom-nav__badge" aria-label="Library sync in progress"></span>
				{/if}
			</a>
			<a
				href={versionUpdateAvailable ? '/settings?tab=about' : '/settings'}
				class="musicseerr-bottom-nav__item"
				class:active={isNavActive('/settings')}
				aria-current={isNavActive('/settings') ? 'page' : undefined}
			>
				<Settings />
				<span>Settings</span>
				{#if versionUpdateAvailable}
					<span class="musicseerr-bottom-nav__badge" aria-label="Update available">
						<ArrowUpCircle class="h-3 w-3" />
					</span>
				{/if}
			</a>
		</nav>

		<dialog id="search_modal" class="modal">
			<div class="modal-box overflow-visible">
				<form method="dialog">
					<button class="btn btn-sm btn-circle btn-ghost absolute right-2 top-2" aria-label="Close"
						><X class="h-4 w-4" /></button
					>
				</form>
				<h3 class="font-bold text-lg mb-4">Search</h3>
				<SearchSuggestions
					bind:query={modalQuery}
					onSearch={handleModalSearch}
					onSelect={handleModalSuggestionSelect}
					placeholder="Search albums or artists..."
					autofocus={true}
					id="modal-suggest"
				/>
			</div>
			<form method="dialog" class="modal-backdrop">
				<button aria-label="Close modal">close</button>
			</form>
		</dialog>

		{#if $errorModal.show}
			<dialog class="modal modal-open">
				<div class="modal-box bg-base-200 border border-base-300 shadow-xl max-w-md">
					<button
						class="btn btn-sm btn-circle btn-ghost absolute right-3 top-3 opacity-60 hover:opacity-100"
						onclick={() => errorModal.hide()}
						aria-label="Close"
					>
						<X class="h-4 w-4" />
					</button>

					<div class="flex flex-col items-center text-center pt-2 pb-1">
						<div class="bg-error/10 rounded-full p-3 mb-4">
							<TriangleAlert class="h-8 w-8 text-error" />
						</div>

						<h3 class="text-lg font-bold text-base-content mb-2">
							{$errorModal.title}
						</h3>

						<p class="text-sm text-base-content/70 leading-relaxed">
							{$errorModal.message}
						</p>
					</div>

					{#if $errorModal.details}
						<div class="mt-4 rounded-box bg-base-300/60 border border-base-300 p-4">
							<div class="flex gap-3 items-start">
								<Info class="h-5 w-5 text-info shrink-0 mt-0.5" />
								<p class="text-sm text-base-content/80 leading-relaxed text-left">
									{$errorModal.details}
								</p>
							</div>
						</div>
					{/if}

					<div class="modal-action justify-center mt-5">
						<button class="btn btn-accent btn-sm px-6" onclick={() => errorModal.hide()}>
							Dismiss
						</button>
					</div>
				</div>
				<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
				<!-- svelte-ignore a11y_click_events_have_key_events -->
				<form method="dialog" class="modal-backdrop" onclick={() => errorModal.hide()}>
					<button>close</button>
				</form>
			</dialog>
		{/if}

		{#if playbackToast.visible}
			<div
				class="musicseerr-playback-toast fixed z-50 left-1/2 -translate-x-1/2 transition-all duration-300"
				class:musicseerr-playback-toast--player={playerStore.isPlayerVisible}
			>
				<div
					class="alert {playbackToast.type === 'error'
						? 'alert-error'
						: playbackToast.type === 'warning'
							? 'alert-warning'
							: 'alert-info'} shadow-lg px-4 py-2 min-w-64 max-w-md"
				>
					{#if playbackToast.type === 'error'}
						<X class="h-5 w-5 shrink-0" />
					{:else if playbackToast.type === 'warning'}
						<TriangleAlert class="h-5 w-5 shrink-0" />
					{:else}
						<Info class="h-5 w-5 shrink-0" />
					{/if}
					<span class="text-sm">{playbackToast.message}</span>
					<button
						class="btn btn-ghost btn-xs btn-circle"
						onclick={() => playbackToast.dismiss()}
						aria-label="Dismiss"
					>
						<X class="h-3.5 w-3.5" />
					</button>
				</div>
			</div>
		{/if}

		{#if browser}
			<audio bind:this={audioElement}></audio>
		{/if}

		<Player />
		<CacheSyncIndicator />
		<BatchDownloadIndicator />
		<DiscographyDownloadModal />
		<AddToPlaylistModal bind:this={playlistModalRef} />
	</div>
</QueryProvider>
