<script lang="ts">
	import '../app.css';
	import { browser } from '$app/environment';
	import { goto, beforeNavigate, afterNavigate } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { AUTH_FREE_PATHS } from '$lib/constants';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { logout } from '$lib/utils/logout';
	import { migratePageSourceKeys } from '$lib/stores/musicSource';
	import { errorModal } from '$lib/stores/errorModal';
	import { libraryStore } from '$lib/stores/library';
	import { integrationStore } from '$lib/stores/integration';
	import { downloadsActivity } from '$lib/stores/downloadsActivity.svelte';
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
	import PreviewWidget from '$lib/components/discover/PreviewWidget.svelte';
	import CacheSyncIndicator from '$lib/components/CacheSyncIndicator.svelte';
	import AddToPlaylistModal, {
		registerPlaylistModal,
		unregisterPlaylistModal
	} from '$lib/components/AddToPlaylistModal.svelte';
	import DiscographyDownloadModal from '$lib/components/DiscographyDownloadModal.svelte';
	import BatchDownloadIndicator from '$lib/components/BatchDownloadIndicator.svelte';
	import { syncStatus } from '$lib/stores/syncStatus.svelte';
	import SidebarServices from '$lib/components/SidebarServices.svelte';
	import DegradedBanner from '$lib/components/DegradedBanner.svelte';
	import ServiceHealthIndicator from '$lib/components/ServiceHealthIndicator.svelte';
	import VersionOverlays from '$lib/components/VersionOverlays.svelte';
	import SearchSuggestions from '$lib/components/SearchSuggestions.svelte';
	import Footer from '$lib/components/Footer.svelte';
	import type { SuggestResult } from '$lib/types';
	import { onMount, onDestroy, untrack } from 'svelte';
	import { cancelPendingImages } from '$lib/utils/lazyImage';
	import { abortAllPageRequests } from '$lib/utils/navigationAbort';
	import { pendingApprovalCountStore } from '$lib/stores/pendingApprovalCountStore.svelte';
	import { nowPlayingStore } from '$lib/stores/nowPlayingSessions.svelte';
	import { nowPlayingReporter } from '$lib/stores/nowPlayingReporter.svelte';
	import { createNavigationProgressController } from '$lib/utils/navigationProgress';
	import { fromStore } from 'svelte/store';
	import {
		Settings,
		Search,
		House,
		Compass,
		Menu,
		Download,
		PanelLeft,
		TriangleAlert,
		Info,
		X,
		UserRound,
		Inbox,
		ListMusic,
		ArrowUpCircle,
		LogOut,
		ShieldCheck,
		Heart,
		LibraryBig,
		Cog
	} from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import QueryProvider from '$lib/queries/QueryProvider.svelte';
	import NewReleasesNavBadge from '$lib/components/NewReleasesNavBadge.svelte';
	import ConcertsNavBadge from '$lib/components/ConcertsNavBadge.svelte';
	import { createFollowingEvents } from '$lib/queries/following/FollowingEvents';
	import { createLibraryActivityEvents } from '$lib/queries/library/LibraryActivityEvents';
	import LibraryActivityStrip from '$lib/components/library/LibraryActivityStrip.svelte';

	migratePageSourceKeys();

	let { children }: { children: Snippet } = $props();

	const followingEvents = createFollowingEvents();
	const libraryActivityEvents = createLibraryActivityEvents();

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
	});

	let cleanupResumeListeners: (() => void) | null = null;

	function deferInit(fn: () => void): () => void {
		if ('requestIdleCallback' in window) {
			const id = window.requestIdleCallback(fn, { timeout: 2000 });
			return () => window.cancelIdleCallback(id);
		}
		const id = setTimeout(fn, 100);
		return () => clearTimeout(id);
	}

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
		document.addEventListener('keydown', handleGlobalKeydown);
		if (playlistModalRef) registerPlaylistModal(playlistModalRef);
	});

	// Everything auth-gated must track the session reactively, not be checked once at
	// mount: an in-app login/logout is a goto() that never remounts this layout, so a
	// mount-time check left integrations disabled (and these services stopped) until a
	// hard refresh (#155). The bodies run untracked so only the auth flag re-triggers
	// them - nowPlayingReporter.start() synchronously reads player $state, which would
	// otherwise restart every service on each play/pause.
	$effect(() => {
		const sessionUserId = authStore.user?.id ?? null;
		untrack(() => libraryStore.setSession(sessionUserId));
		if (sessionUserId) {
			const cancelDeferred = untrack(() => {
				// integration status feeds the home entry cards and the services panel
				// (only some pages call ensureLoaded themselves)
				void integrationStore.ensureLoaded();
				return deferInit(() => {
					initCacheTTLs();
					void imageSettingsStore.load();
					void restorePlayerSession();
					void scrobbleManager.init();
					syncStatus.connect();
					downloadsActivity.start();
				});
			});
			return () => {
				cancelDeferred();
				downloadsActivity.stop();
				syncStatus.disconnect();
			};
		} else {
			untrack(() => {
				integrationStore.reset();
				downloadsActivity.stop();
				syncStatus.disconnect();
			});
		}
	});

	$effect(() => {
		if (!authStore.isAuthenticated) return;
		untrack(() => {
			followingEvents.start();
			libraryActivityEvents.start(authStore.isAdmin);
			// presence is server-driven now (the backend polls upstream servers itself),
			// so it no longer waits on integration status
			nowPlayingStore.start();
			nowPlayingReporter.start();
		});
		return () => {
			followingEvents.stop();
			libraryActivityEvents.stop();
			nowPlayingStore.stop();
			nowPlayingReporter.stop();
		};
	});

	$effect(() => {
		if (!authStore.isAdmin) return;
		untrack(() => pendingApprovalCountStore.startPolling());
		return () => pendingApprovalCountStore.stopPolling();
	});

	onDestroy(() => {
		navigationProgress.cleanup();
		cleanupResumeListeners?.();
		cleanupResumeListeners = null;
		if (browser) {
			document.removeEventListener('keydown', handleGlobalKeydown);
		}
		downloadsActivity.stop();
		syncStatus.disconnect();
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

	function isLibraryNavActive(): boolean {
		return isNavActive('/library') && !isNavActive('/library/management');
	}

	const integrations = fromStore(integrationStore);
	const downloadClientConfigured = $derived(
		integrations.current.download_client || !integrations.current.loaded
	);
	const showAppShell = $derived(!AUTH_FREE_PATHS.some((p) => page.url.pathname.startsWith(p)));
</script>

<QueryProvider>
	<div data-testid="app-shell" data-theme="dark" class="droppedneedle-app-shell">
		{#if showNavigationProgress}
			<div class="fixed top-0 left-0 right-0 z-120 pointer-events-none">
				<progress class="progress progress-primary w-full h-1"></progress>
			</div>
		{/if}

		{#if showAppShell}
			<DegradedBanner />
			<VersionOverlays bind:updateAvailable={versionUpdateAvailable} />

			<div class="drawer md:drawer-open">
				<input id="main-drawer" type="checkbox" class="drawer-toggle" />

				<div class="drawer-content flex min-w-0 flex-col isolate">
					<div
						class="droppedneedle-topbar navbar bg-base-100/95 backdrop-blur shadow-sm sticky top-0 z-50"
					>
						<div class="navbar-start w-auto">
							<a href="/" class="btn btn-ghost px-2 max-xs:hidden sm:px-4" aria-label="Home">
								<img src="/logo_wide.png" alt="DroppedNeedle" class="h-8 hidden sm:block" />
								<img src="/logo_icon.png" alt="DroppedNeedle" class="h-8 block sm:hidden" />
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
							<ServiceHealthIndicator />
							<a href="/profile" class="btn btn-ghost btn-circle btn-md" aria-label="Profile">
								{#if authStore.user?.avatar_url}
									<img
										src={authStore.user.avatar_url}
										alt="Profile"
										class="h-7 w-7 rounded-full object-cover"
									/>
								{:else}
									<UserRound class="h-6 w-6" />
								{/if}
							</a>
						</div>
					</div>

					<LibraryActivityStrip />

					<div
						class="droppedneedle-main-content flex-1"
						class:droppedneedle-player-visible={playerStore.isPlayerVisible}
					>
						{@render children()}
						<Footer />
					</div>
				</div>

				<div class="drawer-side hidden md:block is-drawer-close:overflow-visible">
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

							<li>
								<a
									href="/library"
									class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
									class:menu-active={isLibraryNavActive()}
									aria-current={isLibraryNavActive() ? 'page' : undefined}
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
									href="/downloads"
									class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
									data-tip="Downloads"
								>
									<div class="relative">
										<Download class="h-6 w-6" />
										{#if downloadsActivity.isActive}
											<span
												class="absolute -top-1.5 -right-2 badge badge-primary badge-xs h-4 min-w-4 animate-pulse px-1"
												aria-label="{downloadsActivity.count} active downloads"
											>
												{downloadsActivity.count}
											</span>
										{/if}
									</div>
									<span class="is-drawer-close:hidden">Downloads</span>
								</a>
							</li>

							<li>
								<a
									href="/following"
									class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
									class:menu-active={isNavActive('/following')}
									aria-current={isNavActive('/following') ? 'page' : undefined}
									data-tip="Following"
								>
									<div class="relative">
										<Heart class="h-6 w-6" />
										<!-- overlapping badge pair (U8): releases left, concerts right -->
										<ConcertsNavBadge />
										<NewReleasesNavBadge />
									</div>
									<span class="is-drawer-close:hidden">Following</span>
								</a>
							</li>

							{#if downloadClientConfigured}
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

							<SidebarServices />

							{#if downloadClientConfigured}
								<div class="divider my-0"></div>
							{/if}

							{#if downloadClientConfigured}
								<li>
									<a
										href="/requests"
										class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
										data-tip="Requests"
									>
										<Inbox class="h-6 w-6" />
										<span class="is-drawer-close:hidden">Requests</span>
									</a>
								</li>
							{/if}

							{#if authStore.isAdmin}
								<div class="divider my-0"></div>
								<li class="menu-title is-drawer-close:hidden px-3 pb-1 pt-2">
									<span
										class="font-mono text-[0.65rem] uppercase tracking-[0.18em] text-base-content/40"
										>Admin</span
									>
								</li>
								<li>
									<a
										href="/library/management"
										class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
										class:menu-active={isNavActive('/library/management')}
										aria-current={isNavActive('/library/management') ? 'page' : undefined}
										aria-label="Library Management"
										data-tip="Library Management"
									>
										<div class="relative h-6 w-6">
											<LibraryBig class="h-6 w-6" />
											<span
												class="absolute -bottom-1.5 -right-1.5 grid h-4 w-4 place-items-center rounded-full bg-base-200 text-library-manage"
											>
												<Cog class="h-3 w-3" />
											</span>
										</div>
										<span class="is-drawer-close:hidden">Library Management</span>
									</a>
								</li>
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
									onclick={() => void logout()}
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

		<nav
			class="droppedneedle-bottom-nav md:hidden"
			style:display={showAppShell ? undefined : 'none'}
			aria-label="Primary navigation"
		>
			<a
				href="/"
				class="droppedneedle-bottom-nav__item"
				class:active={currentPath === '/'}
				aria-current={currentPath === '/' ? 'page' : undefined}
			>
				<House />
				<span>Home</span>
			</a>
			<a
				href="/discover"
				class="droppedneedle-bottom-nav__item"
				class:active={isNavActive('/discover')}
				aria-current={isNavActive('/discover') ? 'page' : undefined}
			>
				<Compass />
				<span>Discover</span>
			</a>
			<button
				type="button"
				class="droppedneedle-bottom-nav__item"
				class:active={isNavActive('/search')}
				onclick={() => (document.getElementById('search_modal') as HTMLDialogElement)?.showModal()}
				aria-current={isNavActive('/search') ? 'page' : undefined}
			>
				<Search />
				<span>Search</span>
			</button>
			<a
				href="/library"
				class="droppedneedle-bottom-nav__item"
				class:active={isNavActive('/library')}
				aria-current={isNavActive('/library') ? 'page' : undefined}
			>
				<Menu />
				<span>Library</span>
				{#if syncStatus.isActive}
					<span class="droppedneedle-bottom-nav__badge" aria-label="Library sync in progress"
					></span>
				{/if}
			</a>
			<a
				href={versionUpdateAvailable ? '/settings?tab=about' : '/settings'}
				class="droppedneedle-bottom-nav__item"
				class:active={isNavActive('/settings')}
				aria-current={isNavActive('/settings') ? 'page' : undefined}
			>
				<Settings />
				<span>Settings</span>
				{#if versionUpdateAvailable}
					<span class="droppedneedle-bottom-nav__badge" aria-label="Update available">
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
				class="droppedneedle-playback-toast fixed z-50 left-1/2 -translate-x-1/2 transition-all duration-300"
				class:droppedneedle-playback-toast--player={playerStore.isPlayerVisible}
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
		<PreviewWidget />
		<CacheSyncIndicator />
		<BatchDownloadIndicator />
		<DiscographyDownloadModal />
		<AddToPlaylistModal bind:this={playlistModalRef} />
	</div>
</QueryProvider>
