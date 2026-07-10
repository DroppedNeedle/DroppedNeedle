<script lang="ts">
	/*
	 * PlayerStage — the full-screen immersive player. Blurred-artwork backdrop.
	 * Mobile: art + metadata + transport stacked above the Queue / Lyrics / EQ panel.
	 * Desktop (lg+): cinematic two-column layout — large centred art + metadata on
	 * the left, full-height panel on the right, and a full-width transport band
	 * pinned along the bottom.
	 *
	 * Pure view: playback via playerStore, open/tab state via playerUi.
	 */
	import { playerStore } from '$lib/stores/player.svelte';
	import { playerUi, type StageTab } from '$lib/stores/playerUi.svelte';
	import { playbackToast } from '$lib/stores/playbackToast.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import AudioQualityBadge from '$lib/components/AudioQualityBadge.svelte';
	import TransportControls from '$lib/components/player/TransportControls.svelte';
	import SeekBar from '$lib/components/player/SeekBar.svelte';
	import VolumeControl from '$lib/components/player/VolumeControl.svelte';
	import PlaybackSourceIdentity from '$lib/components/player/PlaybackSourceIdentity.svelte';
	import ScrobbleStatus from '$lib/components/player/ScrobbleStatus.svelte';
	import QueueList from '$lib/components/player/QueueList.svelte';
	import LyricsView from '$lib/components/player/LyricsView.svelte';
	import EqControls from '$lib/components/player/EqControls.svelte';
	import { openGlobalPlaylistModal } from '$lib/components/AddToPlaylistModal.svelte';
	import type { LyricLine } from '$lib/types';
	import {
		ChevronDown,
		Disc3,
		ListMusic,
		Music2,
		SlidersHorizontal,
		Shuffle,
		Trash2,
		X,
		Heart
	} from 'lucide-svelte';

	interface Props {
		supportsLyrics?: boolean;
		lyricsText?: string;
		lyricLines?: LyricLine[];
		lyricsSynced?: boolean;
		lyricsLoading?: boolean;
		lyricsError?: boolean;
	}

	let {
		supportsLyrics = false,
		lyricsText = '',
		lyricLines = [],
		lyricsSynced = false,
		lyricsLoading = false,
		lyricsError = false
	}: Props = $props();

	let stageEl: HTMLElement | undefined = $state();
	let coverImgError = $state(false);
	let lastCoverKey = '';
	let touchStartY: number | null = null;

	const MBID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

	function isAlbumLinkable(id: string | undefined): boolean {
		return !!id && MBID_RE.test(id);
	}

	const nowPlayingCoverUrl = $derived.by(() => {
		const np = playerStore.nowPlaying;
		if (!np) return null;
		return getCoverUrl(np.coverUrl, np.albumId);
	});

	$effect(() => {
		const np = playerStore.nowPlaying;
		if (!np) return;
		const key = `${np.albumId}:${np.coverUrl ?? ''}`;
		if (key !== lastCoverKey) {
			lastCoverKey = key;
			coverImgError = false;
		}
	});

	// if lyrics stop being available while the lyrics tab is up, fall back to the queue
	$effect(() => {
		if (playerUi.stageTab === 'lyrics' && !supportsLyrics) {
			playerUi.setTab('queue');
		}
	});

	// focus + scroll lock while open
	$effect(() => {
		if (playerUi.stageOpen) {
			document.body.classList.add('overflow-hidden');
			queueMicrotask(() => stageEl?.focus());
		} else {
			document.body.classList.remove('overflow-hidden');
		}
		return () => document.body.classList.remove('overflow-hidden');
	});

	function close(): void {
		playerUi.closeStage();
	}

	function handleKeydown(e: KeyboardEvent): void {
		if (e.key === 'Escape') {
			e.stopPropagation();
			close();
		}
	}

	function handleTouchStart(e: TouchEvent): void {
		touchStartY = e.touches[0]?.clientY ?? null;
	}

	function handleTouchEnd(e: TouchEvent): void {
		if (touchStartY === null) return;
		const endY = e.changedTouches[0]?.clientY ?? touchStartY;
		if (endY - touchStartY > 80) close();
		touchStartY = null;
	}

	function handleClearQueue(): void {
		playerStore.clearQueue();
		playbackToast.show('Upcoming queue cleared', 'info');
	}

	const tabs = $derived.by(() => {
		const t: { id: StageTab; label: string; icon: typeof ListMusic }[] = [
			{ id: 'queue', label: 'Queue', icon: ListMusic }
		];
		if (supportsLyrics) t.push({ id: 'lyrics', label: 'Lyrics', icon: Music2 });
		t.push({ id: 'eq', label: 'EQ', icon: SlidersHorizontal });
		return t;
	});
</script>

{#if playerUi.stageOpen && playerStore.nowPlaying}
	<div
		bind:this={stageEl}
		class="dn-stage fixed inset-0 z-[70] flex flex-col outline-none"
		role="dialog"
		aria-modal="true"
		aria-label="Now playing"
		tabindex="-1"
		onkeydown={handleKeydown}
	>
		<!-- backdrop -->
		<div class="dn-stage__backdrop" aria-hidden="true">
			{#if nowPlayingCoverUrl && !coverImgError}
				<img src={nowPlayingCoverUrl} alt="" class="dn-stage__backdrop-img" />
			{/if}
			<div class="dn-stage__backdrop-wash"></div>
		</div>

		<!-- header (swipe-down zone on touch) -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<header
			class="relative z-10 flex items-center gap-3 px-4 pt-[calc(0.75rem+var(--ms-safe-top))] pb-2 sm:px-6"
			ontouchstart={handleTouchStart}
			ontouchend={handleTouchEnd}
		>
			<button class="btn btn-ghost btn-sm btn-circle" onclick={close} aria-label="Collapse player">
				<ChevronDown class="h-5 w-5" />
			</button>
			<div class="min-w-0 flex-1 text-center">
				<p class="text-[0.65rem] font-bold uppercase tracking-[0.18em] opacity-50">Now playing</p>
				<p class="truncate text-xs opacity-70">
					{playerStore.nowPlaying.albumName}
				</p>
			</div>
			<button
				class="btn btn-ghost btn-sm btn-circle opacity-60 hover:opacity-100"
				onclick={() => {
					playerStore.stop();
					close();
				}}
				aria-label="Stop and close player"
			>
				<X class="h-4 w-4" />
			</button>
		</header>

		<!-- body -->
		<div
			class="relative z-10 mx-auto grid w-full max-w-7xl flex-1 gap-6 overflow-y-auto px-4 pb-[calc(1.25rem+var(--ms-safe-bottom))] sm:px-8 lg:min-h-0 lg:max-w-[88rem] lg:grid-cols-[minmax(0,1fr)_minmax(0,26rem)] lg:gap-12 lg:overflow-hidden lg:pb-0"
		>
			<!-- left: artwork + meta (+ transport on mobile) -->
			<div
				class="flex min-h-0 min-w-0 flex-col items-center justify-center gap-5 pt-2 lg:gap-6 lg:pt-0"
			>
				<div class="dn-stage__art" style="view-transition-name: dn-now-art">
					{#if nowPlayingCoverUrl && !coverImgError}
						<img
							src={nowPlayingCoverUrl}
							alt={playerStore.nowPlaying.albumName}
							onerror={() => (coverImgError = true)}
						/>
					{:else}
						<div class="flex h-full w-full items-center justify-center bg-base-200">
							<Disc3 class="h-24 w-24 text-base-content/15" />
						</div>
					{/if}
				</div>

				<div class="w-full max-w-xl text-center lg:max-w-[min(52vh,30rem)] lg:text-left">
					<h2 class="hero-title truncate font-display text-2xl font-bold sm:text-3xl lg:text-4xl">
						{playerStore.nowPlaying.trackName || playerStore.nowPlaying.albumName}
					</h2>
					<p class="mt-1 truncate text-sm opacity-70 sm:text-base">
						{#if isAlbumLinkable(playerStore.nowPlaying.albumId)}
							<a
								href="/album/{playerStore.nowPlaying.albumId}"
								class="hover:underline"
								onclick={close}>{playerStore.nowPlaying.albumName}</a
							>
						{:else}
							{playerStore.nowPlaying.albumName}
						{/if}
						·
						{#if playerStore.nowPlaying.artistId}
							<a
								href="/artist/{playerStore.nowPlaying.artistId}"
								class="hover:underline"
								onclick={close}>{playerStore.nowPlaying.artistName}</a
							>
						{:else}
							{playerStore.nowPlaying.artistName}
						{/if}
					</p>

					<div class="mt-2 flex flex-wrap items-center justify-center gap-3 lg:justify-start">
						{#if playerStore.currentQueueItem}
							<button
								class="btn btn-ghost btn-xs btn-circle"
								onclick={() => openGlobalPlaylistModal([playerStore.currentQueueItem!])}
								aria-label="Add current track to a playlist"
								title="Add to playlist"
							>
								<Heart class="h-4 w-4" />
							</button>
						{/if}
						{#if playerStore.hasQueue}
							<span class="text-xs opacity-40"
								>Track {playerStore.currentTrackNumber} of {playerStore.queueLength}</span
							>
						{/if}
						<!-- on desktop these live in the bottom transport band instead -->
						<span class="contents lg:hidden">
							{#if playerStore.nowPlaying.format}
								<AudioQualityBadge codec={playerStore.nowPlaying.format} />
							{/if}
							<PlaybackSourceIdentity />
							<ScrobbleStatus />
						</span>
					</div>
					{#if playerStore.playbackState === 'error'}
						<p class="mt-2 text-sm text-error">This track isn't available right now.</p>
					{/if}
				</div>

				<!-- mobile transport stack — replaced by the bottom band on lg+ -->
				<div class="w-full max-w-xl lg:hidden">
					<SeekBar showHint size="stage" />
				</div>

				<div class="flex w-full max-w-xl items-center justify-center lg:hidden">
					<TransportControls size="stage" />
				</div>

				<div class="hidden items-center justify-center sm:flex lg:hidden">
					<VolumeControl width="w-36" />
				</div>
			</div>

			<!-- right: queue / lyrics / eq -->
			<div class="flex min-h-0 min-w-0 flex-col pb-4 lg:py-5">
				<div class="mb-3 flex min-w-0 flex-wrap items-center justify-between gap-2">
					<div class="join">
						{#each tabs as tab (tab.id)}
							<button
								class="btn btn-sm join-item {playerUi.stageTab === tab.id
									? 'btn-accent'
									: 'btn-ghost bg-base-content/5'}"
								onclick={() => playerUi.setTab(tab.id)}
								aria-pressed={playerUi.stageTab === tab.id}
							>
								<tab.icon class="h-3.5 w-3.5" />
								{tab.label}
							</button>
						{/each}
					</div>

					{#if playerUi.stageTab === 'queue' && playerStore.queue.length > 0}
						<div class="flex items-center gap-1">
							<button
								class="btn btn-ghost btn-sm btn-circle"
								class:text-accent={playerStore.shuffleEnabled}
								class:opacity-50={!playerStore.shuffleEnabled}
								onclick={() => playerStore.toggleShuffle()}
								aria-label="Toggle shuffle"
							>
								<Shuffle class="h-3.5 w-3.5" />
							</button>
							<button class="btn btn-ghost btn-sm gap-1 text-error" onclick={handleClearQueue}>
								<Trash2 class="h-3.5 w-3.5" />
								Clear
							</button>
						</div>
					{/if}
				</div>

				<div class="dn-stage__panel flex min-h-64 flex-1 flex-col overflow-hidden">
					{#if playerUi.stageTab === 'queue'}
						<div class="flex-1 overflow-y-auto">
							<QueueList active={playerUi.stageOpen} />
							{#if playerStore.upcomingQueueLength <= 1}
								<div class="hidden px-6 py-8 text-center lg:block">
									<p class="font-mono text-[0.65rem] uppercase tracking-[0.18em] opacity-40">
										Queue is short
									</p>
									<p class="mt-1.5 text-xs opacity-50">
										Add from the album page or start a station
									</p>
								</div>
							{/if}
						</div>
						{#if playerStore.queue.length > 0}
							<div class="border-t border-base-content/10 p-2.5 text-center text-xs opacity-50">
								{playerStore.upcomingQueueLength} track{playerStore.upcomingQueueLength === 1
									? ''
									: 's'} upcoming
							</div>
						{/if}
					{:else if playerUi.stageTab === 'lyrics'}
						<LyricsView
							{lyricsText}
							lines={lyricLines}
							isSynced={lyricsSynced}
							isLoading={lyricsLoading}
							hasError={lyricsError}
							currentTime={playerStore.progress}
							size="stage"
						/>
						{#if lyricsSynced}
							<div class="border-t border-base-content/10 px-4 py-2">
								<span class="badge badge-xs badge-primary">Synced</span>
							</div>
						{/if}
					{:else if playerUi.stageTab === 'eq'}
						<!-- pan-y + overscroll-contain: the panel may scroll vertically, but must
						     not chain/steal the band drags (tracks themselves are touch-action:none) -->
						<div class="touch-pan-y overflow-y-auto overscroll-contain p-4">
							<EqControls trackHeight={200} />
						</div>
					{/if}
				</div>
			</div>
		</div>

		<!-- desktop transport band: full-width, spans both columns -->
		<div class="dn-stage__band relative z-10 hidden lg:block">
			<div class="mx-auto w-full max-w-[88rem] px-8 pt-3 pb-[calc(1rem+var(--ms-safe-bottom))]">
				<SeekBar showHint size="stage" />
				<div class="mt-1.5 grid grid-cols-[1fr_auto_1fr] items-center gap-8">
					<div class="min-w-0">
						<p class="truncate text-sm font-medium">
							{playerStore.nowPlaying.trackName || playerStore.nowPlaying.albumName}
						</p>
						<p class="truncate font-mono text-[0.65rem] uppercase tracking-[0.14em] opacity-50">
							{playerStore.nowPlaying.artistName}
						</p>
					</div>
					<TransportControls size="stage" />
					<div class="flex min-w-0 items-center justify-end gap-4">
						<VolumeControl width="w-36" />
						{#if playerStore.nowPlaying.format}
							<AudioQualityBadge codec={playerStore.nowPlaying.format} />
						{/if}
						<PlaybackSourceIdentity />
						<ScrobbleStatus />
					</div>
				</div>
			</div>
		</div>
	</div>
{/if}

<style>
	.dn-stage {
		background: var(--color-base-100);
		animation: dn-stage-in var(--dn-dur-base) var(--ease-spring);
		/* long queue titles must truncate, never widen the player sideways */
		overflow-x: clip;
	}

	@keyframes dn-stage-in {
		from {
			opacity: 0;
			transform: translateY(2.5rem);
		}
		to {
			opacity: 1;
			transform: none;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.dn-stage {
			animation: none;
		}
	}

	.dn-stage__backdrop {
		position: absolute;
		inset: 0;
		overflow: hidden;
	}
	.dn-stage__backdrop-img {
		position: absolute;
		inset: -10%;
		width: 120%;
		height: 120%;
		object-fit: cover;
		filter: blur(72px) saturate(1.2);
		opacity: 0.42;
		transform: scale(1.1);
	}
	.dn-stage__backdrop-wash {
		position: absolute;
		inset: 0;
		background:
			radial-gradient(
				ellipse at 50% 105%,
				rgb(var(--dn-now-tint, 21 23 19) / 0.35),
				transparent 62%
			),
			radial-gradient(
				ellipse at 30% 20%,
				oklch(from var(--color-base-100) l c h / 0.25),
				transparent 60%
			),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-100) l c h / 0.55) 0%,
				oklch(from var(--color-base-100) l c h / 0.82) 62%,
				var(--color-base-100) 100%
			);
	}

	.dn-stage__art {
		width: min(58vw, 22rem);
		aspect-ratio: 1;
		/* flex columns may compress children vertically, squashing the square
		   into a landscape sliver — never shrink the record */
		flex: 0 0 auto;
		overflow: hidden;
		border-radius: var(--dn-radius-lg);
		box-shadow: var(--dn-shadow-4);
		outline: 1px solid var(--dn-hairline);
	}
	@media (min-width: 1024px) {
		.dn-stage__art {
			width: min(52vh, 30rem);
		}
	}
	.dn-stage__art img {
		width: 100%;
		height: 100%;
		object-fit: cover;
	}

	.dn-stage__band {
		border-top: 1px solid var(--dn-hairline);
		background: oklch(from var(--color-base-100) l c h / 0.55);
		backdrop-filter: blur(20px) saturate(1.05);
		-webkit-backdrop-filter: blur(20px) saturate(1.05);
	}

	.dn-stage__panel {
		border-radius: var(--dn-radius-md);
		background: oklch(from var(--color-base-200) l c h / 0.6);
		backdrop-filter: blur(20px) saturate(1.05);
		-webkit-backdrop-filter: blur(20px) saturate(1.05);
		border: 1px solid var(--dn-hairline);
		box-shadow: var(--dn-shadow-2);
	}
</style>
