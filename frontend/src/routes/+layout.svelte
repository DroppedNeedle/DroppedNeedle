<script lang="ts">
	import '../app.css';
	import { browser } from '$app/environment';
	import { beforeNavigate, afterNavigate, onNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import { API, AUTH_FREE_PATHS } from '$lib/constants';
	import { api } from '$lib/api/client';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { appearance } from '$lib/stores/appearance.svelte';
	import AppSidebar from '$lib/shell/AppSidebar.svelte';
	import AppTopbar from '$lib/shell/AppTopbar.svelte';
	import AppBottomNav from '$lib/shell/AppBottomNav.svelte';
	import CommandPalette from '$lib/shell/CommandPalette.svelte';
	import NowPlayingPanel from '$lib/shell/NowPlayingPanel.svelte';
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
	import DegradedBanner from '$lib/components/DegradedBanner.svelte';
	import VersionOverlays from '$lib/components/VersionOverlays.svelte';
	import Footer from '$lib/components/Footer.svelte';
	import { onMount, onDestroy } from 'svelte';
	import { cancelPendingImages } from '$lib/utils/lazyImage';
	import { abortAllPageRequests } from '$lib/utils/navigationAbort';
	import { pendingApprovalCountStore } from '$lib/stores/pendingApprovalCountStore.svelte';
	import { nowPlayingStore } from '$lib/stores/nowPlayingSessions.svelte';
	import { nowPlayingReporter } from '$lib/stores/nowPlayingReporter.svelte';
	import { createNavigationProgressController } from '$lib/utils/navigationProgress';
	import { TriangleAlert, Info, X } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import QueryProvider from '$lib/queries/QueryProvider.svelte';
	import { createFollowingEvents } from '$lib/queries/following/FollowingEvents';

	migratePageSourceKeys();

	let { children }: { children: Snippet } = $props();

	const followingEvents = createFollowingEvents();

	let audioElement = $state<HTMLAudioElement | undefined>(undefined);
	let playlistModalRef: AddToPlaylistModal | undefined = $state(undefined);
	let showNavigationProgress = $state(false);
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
		navigationProgress.finish();
		libraryStore.refreshIfStale(10_000);
		// on phones the sidebar is an overlay app drawer — close it after navigating
		if (browser && window.innerWidth < 768) {
			const drawer = document.getElementById('main-drawer') as HTMLInputElement | null;
			if (drawer?.checked) drawer.checked = false;
		}
	});

	// soft cross-fade between pages (view transitions); skipped under reduced motion
	onNavigate((navigation) => {
		if (!browser) return;
		const doc = document as Document & {
			startViewTransition?: (cb: () => Promise<void>) => unknown;
		};
		if (typeof doc.startViewTransition !== 'function') return;
		if (
			document.documentElement.dataset.dnMotion === 'reduced' ||
			window.matchMedia('(prefers-reduced-motion: reduce)').matches
		) {
			return;
		}
		return new Promise((resolve) => {
			doc.startViewTransition!(async () => {
				resolve();
				await navigation.complete;
			});
		});
	});

	let cleanupResumeListeners: (() => void) | null = null;

	onMount(() => {
		appearance.init();

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
			if (authStore.isAdmin) pendingApprovalCountStore.startPolling();
			if (authStore.isAuthenticated) followingEvents.start();
			syncStatus.connect();
			downloadsActivity.start();
		});
		// load integration status once for the whole app - the home entry cards and the
		// services panel depend on it (only some pages call ensureLoaded themselves)
		void integrationStore.ensureLoaded();
		// presence is server-driven now (the backend polls upstream servers itself), so
		// it no longer waits on integration status
		if (authStore.isAuthenticated) {
			nowPlayingStore.start();
			nowPlayingReporter.start();
		}
	});

	onDestroy(() => {
		navigationProgress.cleanup();
		cleanupResumeListeners?.();
		cleanupResumeListeners = null;
		if (browser) {
			document.removeEventListener('keydown', handleGlobalKeydown);
		}
		pendingApprovalCountStore.stopPolling();
		followingEvents.stop();
		downloadsActivity.stop();
		syncStatus.disconnect();
		nowPlayingStore.stop();
		nowPlayingReporter.stop();
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

	const showAppShell = $derived(!AUTH_FREE_PATHS.some((p) => page.url.pathname.startsWith(p)));

	// raw poll: QueryClient context lives below this component, so TanStack isn't available here
	let libraryScanActive = $state(false);
	$effect(() => {
		if (!showAppShell) return;
		let cancelled = false;
		const poll = async () => {
			try {
				const s = await api.global.get<{ status: string }>(API.library.scanStatus());
				if (!cancelled) libraryScanActive = s.status === 'scanning';
			} catch {
				/* ignore - nav dot is best-effort */
			}
		};
		void poll();
		const timer = setInterval(poll, 5000);
		return () => {
			cancelled = true;
			clearInterval(timer);
		};
	});
</script>

<QueryProvider>
	<div data-theme={appearance.resolvedTheme} class="droppedneedle-app-shell">
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
					<AppTopbar />

					<div class="flex min-w-0 flex-1 items-start">
						<div
							class="droppedneedle-main-content min-w-0 flex-1"
							class:droppedneedle-player-visible={playerStore.isPlayerVisible}
						>
							{@render children()}
							<Footer />
						</div>

						<NowPlayingPanel />
					</div>
				</div>

				<div class="drawer-side is-drawer-close:overflow-visible">
					<label for="main-drawer" aria-label="close sidebar" class="drawer-overlay"></label>
					<AppSidebar {versionUpdateAvailable} {libraryScanActive} />
				</div>
			</div>
		{:else}
			{@render children()}
		{/if}

		<AppBottomNav {versionUpdateAvailable} {libraryScanActive} />

		{#if showAppShell}
			<CommandPalette />
		{/if}

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
					class="flex min-w-64 max-w-md items-center gap-3 rounded-2xl border border-base-content/10 bg-base-200/95 px-4 py-2.5 shadow-xl backdrop-blur-xl {playbackToast.type ===
					'error'
						? 'text-error'
						: playbackToast.type === 'warning'
							? 'text-warning'
							: 'text-base-content'}"
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
