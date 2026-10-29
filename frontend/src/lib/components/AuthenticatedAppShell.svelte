<script lang="ts">
	import '../../app.css';
	import { browser } from '$app/environment';
	import { beforeNavigate, afterNavigate } from '$app/navigation';
	import { withoutBasePath } from '$lib/utils/basePath';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { migratePageSourceKeys } from '$lib/stores/musicSource';
	import { watchMusicBrainzSourceScope } from '$lib/queries/musicbrainz/sourceScope.svelte';
	import { libraryStore } from '$lib/stores/library';
	import {
		getIntegrationStatusQuery,
		useIntegrationStatus
	} from '$lib/queries/HomeIntegrationStatusQuery.svelte';
	import { loadAdvancedSettings } from '$lib/queries/AdvancedSettingsReads.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { playbackToast } from '$lib/stores/playbackToast.svelte';
	import { scrobbleManager } from '$lib/stores/scrobble.svelte';
	import { serviceStatusStore } from '$lib/stores/serviceStatus';
	import { resumeAudioEngine, setAudioElement } from '$lib/player/audioElement';
	import { eqStore } from '$lib/stores/eq.svelte';
	import Player from '$lib/components/Player.svelte';
	import PreviewWidget from '$lib/components/discover/PreviewWidget.svelte';
	import CacheSyncIndicator from '$lib/components/CacheSyncIndicator.svelte';
	import {
		playlistModalState,
		registerPlaylistModal,
		resetPlaylistModal,
		unregisterPlaylistModal,
		type PlaylistModalHandle
	} from '$lib/stores/playlistModal.svelte';
	import { loadDiscographyModal, loadPlaylistModal } from '$lib/components/lazyComponentLoaders';
	import { discographyDownloadStore } from '$lib/stores/discographyDownload.svelte';
	import { batchDownloadStore } from '$lib/stores/batchDownloadStatus.svelte';
	import BatchDownloadIndicator from '$lib/components/BatchDownloadIndicator.svelte';
	import { syncStatus } from '$lib/stores/syncStatus.svelte';
	import DegradedBanner from '$lib/components/DegradedBanner.svelte';
	import VersionOverlays from '$lib/components/VersionOverlays.svelte';
	import Footer from '$lib/components/Footer.svelte';
	import Toaster from '$lib/ui/Toaster.svelte';
	import { onMount, onDestroy, untrack } from 'svelte';
	import { cancelPendingImages } from '$lib/utils/lazyImage';
	import { abortAllPageRequests } from '$lib/utils/navigationAbort';
	import { nowPlayingStore } from '$lib/stores/nowPlayingSessions.svelte';
	import { nowPlayingReporter } from '$lib/stores/nowPlayingReporter.svelte';
	import { createNavigationProgressController } from '$lib/utils/navigationProgress';
	import type { Component, Snippet } from 'svelte';
	import { createFollowingEvents } from '$lib/queries/following/FollowingEvents';
	import { createLibraryActivityEvents } from '$lib/queries/library/LibraryActivityEvents';
	import { createDownloadEvents } from '$lib/queries/downloads/DownloadEvents';
	import { muxEventStream } from '$lib/queries/events/MuxEventStream';
	import LibraryActivityStrip from '$lib/components/library/LibraryActivityStrip.svelte';
	import TopBar from '$lib/components/shell/TopBar.svelte';
	import SidebarNav from '$lib/components/shell/SidebarNav.svelte';
	import BottomNav from '$lib/components/shell/BottomNav.svelte';
	import MoreNavSheet from '$lib/components/shell/MoreNavSheet.svelte';
	import SearchDialog from '$lib/components/shell/SearchDialog.svelte';
	import ErrorDialog from '$lib/components/shell/ErrorDialog.svelte';
	import PlaybackToast from '$lib/components/shell/PlaybackToast.svelte';
	import { handlePlayerShortcut, restorePlayerSession } from '$lib/components/shell/playerSession';

	migratePageSourceKeys();

	let { children }: { children: Snippet } = $props();
	type PlaylistModalComponent = Component<Record<string, never>, PlaylistModalHandle>;

	const followingEvents = createFollowingEvents();
	const libraryActivityEvents = createLibraryActivityEvents();
	const downloadEvents = createDownloadEvents();

	let audioElement = $state<HTMLAudioElement | undefined>(undefined);
	let PlaylistModal = $state<PlaylistModalComponent | null>(null);
	let DiscographyModal = $state<Component | null>(null);
	let playlistModalRef = $state<PlaylistModalHandle | undefined>(undefined);
	let showNavigationProgress = $state(false);
	let currentPath = $state('/');
	let versionUpdateAvailable = $state(false);
	let searchOpen = $state(false);
	let moreOpen = $state(false);
	// Settings runs as a fixed two-pane app surface: the tab rail and the content
	// pane scroll, the document itself must not.
	const isSettingsPane = $derived(currentPath.startsWith('/settings'));

	// Session-wide reads: the sidebar, page gates, album cards and images use them.
	getIntegrationStatusQuery();
	loadAdvancedSettings();
	const integrations = useIntegrationStatus();
	const viewer = $derived({
		isAdmin: authStore.isAdmin,
		downloadClient: integrations.current.download_client || !integrations.current.loaded
	});

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
			currentPath = withoutBasePath(window.location.pathname);
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
			currentPath = withoutBasePath(window.location.pathname);
		}
		document.addEventListener('keydown', handlePlayerShortcut);
	});

	$effect(() => {
		if (!playlistModalState.shouldMount || PlaylistModal) return;
		let cancelled = false;
		void loadPlaylistModal()
			.then((component) => {
				if (!cancelled) PlaylistModal = component;
			})
			.catch(() => {
				if (cancelled) return;
				resetPlaylistModal();
				playbackToast.show('Could not load the playlist dialog. Try again.', 'error');
			});
		return () => {
			cancelled = true;
		};
	});

	$effect(() => {
		if (!discographyDownloadStore.open || DiscographyModal) return;
		let cancelled = false;
		void loadDiscographyModal()
			.then((component) => {
				if (!cancelled) DiscographyModal = component;
			})
			.catch(() => {
				if (cancelled) return;
				discographyDownloadStore.close();
				playbackToast.show('Could not load the discography dialog. Try again.', 'error');
			});
		return () => {
			cancelled = true;
		};
	});

	$effect(() => {
		if (!playlistModalRef) return;
		const ref = playlistModalRef;
		registerPlaylistModal(ref);
		return () => unregisterPlaylistModal(ref);
	});

	// auth-gated work must track the session reactively: an in-app login/logout is a
	// goto() that never remounts this layout, so mount-time checks left integrations
	// disabled until hard refresh (#155). bodies run untracked so only the auth flag
	// re-triggers them - nowPlayingReporter.start() reads player $state and would
	// otherwise restart every service on each play/pause.
	$effect(() => {
		const sessionUserId = authStore.user?.id ?? null;
		untrack(() => {
			resetPlaylistModal();
			discographyDownloadStore.close();
			batchDownloadStore.clear();
			PlaylistModal = null;
			playlistModalRef = undefined;
			libraryStore.setSession(sessionUserId);
		});
		if (sessionUserId) {
			const cancelDeferred = untrack(() => {
				return deferInit(() => {
					void restorePlayerSession();
					void scrobbleManager.init();
					syncStatus.connect();
				});
			});
			return () => {
				cancelDeferred();
				syncStatus.disconnect();
			};
		} else {
			untrack(() => {
				syncStatus.disconnect();
			});
		}
	});
	$effect(() => {
		if (!authStore.user?.id) return;
		return watchMusicBrainzSourceScope();
	});

	$effect(() => {
		const sessionUserId = authStore.user?.id;
		if (!sessionUserId) return;
		untrack(() => {
			followingEvents.start();
			libraryActivityEvents.start(authStore.isAdmin, sessionUserId);
			downloadEvents.start();
			// presence is server-driven now (the backend polls upstream servers itself),
			// so it no longer waits on integration status
			nowPlayingStore.start();
			nowPlayingReporter.start();
			// The tab's single multiplexed stream; connect last so the gated
			// consumer starts (library activity's admin direct-refresh gate)
			// see a disconnected mux and refresh exactly once via the first
			// open instead of doubling with a direct refresh. syncStatus
			// connects deferred after this on purpose - it has no gate and its
			// seed/generation guard covers either order.
			muxEventStream.connect();
		});
		return () => {
			followingEvents.stop();
			libraryActivityEvents.stop();
			downloadEvents.stop();
			nowPlayingStore.stop();
			nowPlayingReporter.stop();
			muxEventStream.disconnect();
		};
	});

	onDestroy(() => {
		navigationProgress.cleanup();
		cleanupResumeListeners?.();
		cleanupResumeListeners = null;
		if (browser) {
			document.removeEventListener('keydown', handlePlayerShortcut);
		}
		syncStatus.disconnect();
		muxEventStream.disconnect();
		resetPlaylistModal();
		discographyDownloadStore.close();
		batchDownloadStore.clear();
	});
</script>

{#if showNavigationProgress}
	<div class="fixed top-0 left-0 right-0 z-120 pointer-events-none">
		<progress class="progress progress-primary w-full h-1" aria-label="Loading page"></progress>
	</div>
{/if}

<DegradedBanner />
<VersionOverlays bind:updateAvailable={versionUpdateAvailable} />

<div class="drawer md:drawer-open">
	<input id="main-drawer" type="checkbox" class="drawer-toggle" aria-label="Expand sidebar" />

	<div
		class="drawer-content flex min-w-0 flex-col isolate"
		class:droppedneedle-pane-mode={isSettingsPane}
	>
		<TopBar />
		<LibraryActivityStrip />

		<div
			class="droppedneedle-main-content flex-1"
			class:droppedneedle-player-visible={playerStore.isPlayerVisible}
		>
			{@render children()}
			{#if !isSettingsPane}<Footer />{/if}
		</div>
	</div>

	<SidebarNav
		{currentPath}
		{viewer}
		updateAvailable={versionUpdateAvailable}
		onsearch={() => (searchOpen = true)}
	/>
</div>

<BottomNav
	{currentPath}
	{viewer}
	updateAvailable={versionUpdateAvailable}
	onsearch={() => (searchOpen = true)}
	onmore={() => (moreOpen = true)}
/>
<MoreNavSheet
	bind:open={moreOpen}
	{currentPath}
	{viewer}
	updateAvailable={versionUpdateAvailable}
/>
<SearchDialog bind:open={searchOpen} />
<ErrorDialog />
<PlaybackToast />
<Toaster abovePlayer={playerStore.isPlayerVisible} />

{#if browser}
	<audio bind:this={audioElement}></audio>
{/if}

<Player />
<PreviewWidget />
<CacheSyncIndicator />
<BatchDownloadIndicator />
{#if DiscographyModal}
	<DiscographyModal />
{/if}
{#if PlaylistModal}
	<PlaylistModal bind:this={playlistModalRef} />
{/if}

<style>
	.droppedneedle-main-content {
		display: flex;
		flex-direction: column;
		/* fill at least the viewport below the topbar so a short page's footer still anchors to the bottom */
		min-height: calc(100dvh - 4rem - var(--ms-safe-top));
		max-width: 100vw;
		min-width: 0;
		overflow-x: clip;
		padding-bottom: calc(var(--ms-bottom-nav-offset) + var(--ms-page-bottom-gap));
	}

	/* The page inside the shell. Global, so a page's own rules keep winning. */
	:global(.droppedneedle-main-content > *) {
		min-width: 0;
	}

	:global(.droppedneedle-main-content :where(p, li, code, pre)) {
		max-width: 100%;
		overflow-wrap: anywhere;
	}

	:global(.droppedneedle-main-content :where(img, video, canvas, svg)) {
		max-width: 100%;
	}

	/* app headings use the display face, like modal titles */
	:global(.droppedneedle-main-content :is(h1, h2)) {
		font-family: var(--font-display);
	}

	/* structural labels go uppercase; content names keep their case via .hero-title */
	:global(.droppedneedle-main-content :is(h1, h2):not(.hero-title)) {
		text-transform: uppercase;
		letter-spacing: 0.02em;
	}

	.droppedneedle-main-content.droppedneedle-player-visible {
		padding-bottom: calc(
			var(--ms-bottom-nav-offset) + var(--ms-player-height) + var(--ms-page-bottom-gap)
		);
	}

	/* Settings runs as a fixed two-pane app surface at desktop widths: the drawer
	   column locks to the viewport and the page's own panes scroll instead of the
	   document, so the shell stops emitting a third whole-page scrollbar. The
	   footer is omitted on this route, so main-content's fill-the-viewport
	   min-height would force a phantom document overflow; neutralize it and let
	   flexbox distribute the locked height instead. */
	@media (min-width: 1024px) {
		.drawer-content.droppedneedle-pane-mode {
			height: 100dvh;
			overflow: hidden;
		}

		.droppedneedle-pane-mode .droppedneedle-main-content {
			min-height: 0;
		}
	}

	@media (min-width: 768px) {
		.droppedneedle-main-content {
			padding-bottom: 0;
		}

		.droppedneedle-main-content.droppedneedle-player-visible {
			padding-bottom: 6rem;
		}
	}
</style>
