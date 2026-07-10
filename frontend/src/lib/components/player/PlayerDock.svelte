<script lang="ts">
	/*
	 * PlayerDock — the persistent player. On desktop a full-width three-zone
	 * dock (meta / transport / tools); on mobile a compact capsule that opens
	 * the Stage. A hairline progress rail runs along the top edge.
	 *
	 * Everything is driven by playerStore; this component owns no playback state.
	 */
	import { playerStore } from '$lib/stores/player.svelte';
	import { playerUi } from '$lib/stores/playerUi.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { openGlobalPlaylistModal } from '$lib/components/AddToPlaylistModal.svelte';
	import AudioQualityBadge from '$lib/components/AudioQualityBadge.svelte';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';
	import TransportControls from '$lib/components/player/TransportControls.svelte';
	import SeekBar from '$lib/components/player/SeekBar.svelte';
	import VolumeControl from '$lib/components/player/VolumeControl.svelte';
	import PlaybackSourceIdentity from '$lib/components/player/PlaybackSourceIdentity.svelte';
	import ScrobbleStatus from '$lib/components/player/ScrobbleStatus.svelte';
	import {
		X,
		Disc3,
		ListMusic,
		Music2,
		SlidersHorizontal,
		ChevronUp,
		Play,
		Pause,
		SkipForward,
		AlertCircle,
		Heart
	} from 'lucide-svelte';

	interface Props {
		supportsLyrics?: boolean;
	}

	let { supportsLyrics = false }: Props = $props();

	let coverImgError = $state(false);
	let lastCoverKey = '';

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

	const progressPct = $derived(
		playerStore.duration > 0
			? Math.min(100, (playerStore.progress / playerStore.duration) * 100)
			: 0
	);

	function openStage(e?: Event): void {
		e?.stopPropagation();
		playerUi.openStage();
	}
</script>

{#if playerStore.nowPlaying}
	<div
		class="droppedneedle-player-bar dn-dock fixed left-0 right-0 z-50"
		class:dn-dock--hidden={playerUi.stageOpen}
	>
		<!-- top-edge progress rail (visual; interactive seek lives in the seek bar / Stage) -->
		<div class="dn-dock__rail" aria-hidden="true">
			<div class="dn-dock__rail-fill" style="width: {progressPct}%"></div>
		</div>

		<!-- ═══════════ mobile capsule (< sm) ═══════════ -->
		<div
			class="dn-dock__capsule sm:hidden"
			role="button"
			tabindex="0"
			aria-label="Open full-screen player"
			onclick={() => openStage()}
			onkeydown={(e) => {
				if (e.key === 'Enter' || e.key === ' ') {
					e.preventDefault();
					openStage();
				}
			}}
		>
			{#if nowPlayingCoverUrl && !coverImgError}
				<img
					src={nowPlayingCoverUrl}
					alt={playerStore.nowPlaying.albumName}
					class="h-11 w-11 shrink-0 rounded-lg object-cover ring-1 ring-base-content/10"
					onerror={() => (coverImgError = true)}
				/>
			{:else}
				<div class="flex h-11 w-11 shrink-0 items-center justify-center rounded-lg bg-base-200">
					<Disc3 class="h-5 w-5 text-base-content/20" />
				</div>
			{/if}

			<div class="min-w-0 flex-1">
				<p class="truncate text-sm font-semibold">
					{playerStore.nowPlaying.trackName || playerStore.nowPlaying.albumName}
				</p>
				<p class="truncate text-xs opacity-60">{playerStore.nowPlaying.artistName}</p>
				{#if playerStore.playbackState === 'error'}
					<p class="truncate text-xs text-error">This track isn't available right now.</p>
				{/if}
			</div>

			<button
				class="btn btn-circle btn-primary h-10 w-10 shadow-md"
				onclick={(e) => {
					e.stopPropagation();
					if (playerStore.playbackState === 'error') {
						playerStore.stop();
					} else {
						playerStore.togglePlay();
					}
				}}
				aria-label={playerStore.playbackState === 'error'
					? 'Close'
					: playerStore.isPlaying
						? 'Pause'
						: 'Play'}
			>
				{#if playerStore.playbackState === 'error'}
					<AlertCircle class="h-5 w-5" />
				{:else if playerStore.isBuffering}
					<span class="loading loading-spinner loading-sm"></span>
				{:else if playerStore.isPlaying}
					<Pause class="h-5 w-5 fill-current" />
				{:else}
					<Play class="ml-0.5 h-5 w-5 fill-current" />
				{/if}
			</button>

			<button
				class="btn btn-ghost btn-sm btn-circle"
				class:opacity-30={!playerStore.hasNext}
				disabled={!playerStore.hasNext}
				onclick={(e) => {
					e.stopPropagation();
					playerStore.nextTrack();
				}}
				aria-label="Next"
			>
				<SkipForward class="h-4 w-4 fill-current" />
			</button>

			<button
				class="btn btn-ghost btn-xs btn-circle opacity-60"
				onclick={(e) => {
					e.stopPropagation();
					playerStore.stop();
				}}
				aria-label="Close player"
			>
				<X class="h-3.5 w-3.5" />
			</button>
		</div>

		<!-- ═══════════ desktop dock (sm+) ═══════════ -->
		<div
			class="dn-dock__inner mx-auto hidden h-full max-w-screen-2xl items-center gap-4 px-4 sm:flex"
		>
			<!-- zone 1: artwork + meta -->
			<div class="flex w-1/4 min-w-0 flex-none items-center gap-3">
				<button
					class="dn-dock__art group relative shrink-0"
					style:view-transition-name={playerUi.stageOpen ? undefined : 'dn-now-art'}
					onclick={() => openStage()}
					aria-label="Open full-screen player"
				>
					{#if nowPlayingCoverUrl && !coverImgError}
						<img
							src={nowPlayingCoverUrl}
							alt={playerStore.nowPlaying.albumName}
							class="h-14 w-14 rounded-lg object-cover shadow-lg ring-1 ring-base-content/10"
							onerror={() => (coverImgError = true)}
						/>
					{:else}
						<div
							class="flex h-14 w-14 items-center justify-center rounded-lg bg-base-200 shadow-lg"
						>
							<Disc3 class="h-6 w-6 text-base-content/20" />
						</div>
					{/if}
					<span
						class="absolute inset-0 flex items-center justify-center rounded-lg bg-black/45 opacity-0 transition-opacity group-hover:opacity-100"
					>
						<ChevronUp class="h-5 w-5 text-white" />
					</span>
				</button>

				{#if playerStore.isPlaying}
					<div class="hidden lg:block">
						<NowPlayingIndicator size="md" />
					</div>
				{/if}

				<div class="min-w-0 pr-1">
					{#if playerStore.nowPlaying.trackName}
						<p class="truncate text-sm font-semibold">{playerStore.nowPlaying.trackName}</p>
						<p class="truncate text-xs opacity-60">
							{#if isAlbumLinkable(playerStore.nowPlaying.albumId)}
								<a href="/album/{playerStore.nowPlaying.albumId}" class="hover:underline"
									>{playerStore.nowPlaying.albumName}</a
								>
							{:else}
								{playerStore.nowPlaying.albumName}
							{/if}
							-
							{#if playerStore.nowPlaying.artistId}
								<a href="/artist/{playerStore.nowPlaying.artistId}" class="hover:underline"
									>{playerStore.nowPlaying.artistName}</a
								>
							{:else}
								{playerStore.nowPlaying.artistName}
							{/if}
						</p>
					{:else}
						<p class="truncate text-sm font-semibold">
							{#if isAlbumLinkable(playerStore.nowPlaying.albumId)}
								<a href="/album/{playerStore.nowPlaying.albumId}" class="hover:underline"
									>{playerStore.nowPlaying.albumName}</a
								>
							{:else}
								{playerStore.nowPlaying.albumName}
							{/if}
						</p>
						<p class="truncate text-xs opacity-60">
							{#if playerStore.nowPlaying.artistId}
								<a href="/artist/{playerStore.nowPlaying.artistId}" class="hover:underline"
									>{playerStore.nowPlaying.artistName}</a
								>
							{:else}
								{playerStore.nowPlaying.artistName}
							{/if}
						</p>
					{/if}
					<div class="flex items-center gap-2">
						{#if playerStore.hasQueue}
							<p class="truncate text-xs opacity-40">
								Track {playerStore.currentTrackNumber} of {playerStore.queueLength}
							</p>
						{/if}
						{#if playerStore.nowPlaying.format}
							<AudioQualityBadge codec={playerStore.nowPlaying.format} compact />
						{/if}
					</div>
					{#if playerStore.playbackState === 'error'}
						<p class="truncate text-xs text-error">This track isn't available right now.</p>
					{/if}
				</div>
			</div>

			<!-- zone 2: transport + seek -->
			<div class="flex min-w-0 flex-1 flex-col items-center justify-center gap-1">
				<TransportControls size="dock" />
				<div class="w-full max-w-lg">
					<SeekBar showHint size="dock" />
				</div>
			</div>

			<!-- zone 3: tools -->
			<div class="flex w-1/4 flex-none items-center justify-end gap-2 lg:gap-3">
				{#if playerStore.currentQueueItem}
					<div class="tooltip" data-tip="Add to playlist">
						<button
							class="btn btn-ghost btn-sm btn-circle"
							onclick={() => openGlobalPlaylistModal([playerStore.currentQueueItem!])}
							aria-label="Add current track to a playlist"
						>
							<Heart class="h-4 w-4" />
						</button>
					</div>
				{/if}
				<div class="tooltip" data-tip="Queue">
					<button
						class="btn btn-ghost btn-sm btn-circle relative"
						class:text-accent={playerUi.stageOpen && playerUi.stageTab === 'queue'}
						onclick={() => playerUi.toggleStage('queue')}
						aria-label="Open queue"
					>
						<ListMusic class="h-4 w-4" />
						{#if playerStore.upcomingQueueLength > 0}
							<span class="badge badge-xs badge-accent absolute -top-1 -right-1"
								>{playerStore.upcomingQueueLength}</span
							>
						{/if}
					</button>
				</div>

				{#if supportsLyrics}
					<div class="tooltip" data-tip="Lyrics">
						<button
							class="btn btn-ghost btn-sm btn-circle"
							class:text-accent={playerUi.stageOpen && playerUi.stageTab === 'lyrics'}
							onclick={() => playerUi.toggleStage('lyrics')}
							aria-label="Open lyrics"
						>
							<Music2 class="h-4 w-4" />
						</button>
					</div>
				{/if}

				<div
					class="tooltip"
					data-tip={playerStore.nowPlaying?.sourceType === 'youtube'
						? 'EQ unavailable for YouTube'
						: 'Equalizer'}
				>
					<button
						class="btn btn-ghost btn-sm btn-circle"
						class:opacity-30={playerStore.nowPlaying?.sourceType === 'youtube'}
						class:text-accent={playerUi.stageOpen && playerUi.stageTab === 'eq'}
						onclick={() => playerUi.toggleStage('eq')}
						aria-label="Open equalizer"
					>
						<SlidersHorizontal class="h-4 w-4" />
					</button>
				</div>

				<div class="hidden md:block">
					<VolumeControl />
				</div>

				<div class="hidden md:block">
					<ScrobbleStatus />
				</div>

				<div class="hidden md:block">
					<PlaybackSourceIdentity />
				</div>

				<div class="tooltip" data-tip="Full screen">
					<button
						class="btn btn-ghost btn-sm btn-circle"
						onclick={() => openStage()}
						aria-label="Open full-screen player"
					>
						<ChevronUp class="h-4 w-4" />
					</button>
				</div>

				<button
					class="btn btn-ghost btn-xs btn-circle opacity-60 hover:opacity-100"
					onclick={() => playerStore.stop()}
					aria-label="Close player"
				>
					<X class="h-3.5 w-3.5" />
				</button>
			</div>
		</div>
	</div>
{/if}

<style>
	.dn-dock {
		bottom: var(--ms-bottom-nav-offset);
		height: var(--ms-player-height);
		background: oklch(from var(--color-base-200) l c h / 0.92);
		backdrop-filter: blur(18px) saturate(1.05);
		-webkit-backdrop-filter: blur(18px) saturate(1.05);
		border-top: 1px solid var(--dn-hairline);
		box-shadow: 0 -10px 30px rgb(0 0 0 / 0.3);
		transition:
			transform var(--dn-dur-base) var(--ease-spring),
			opacity var(--dn-dur-base) ease;
	}

	/* while the Stage is up the dock slips away; it stays mounted so playback
	   chrome (e.g. the YouTube pop-out) is untouched */
	.dn-dock--hidden {
		transform: translateY(110%);
		opacity: 0;
		pointer-events: none;
	}

	.dn-dock__rail {
		position: absolute;
		top: -1px;
		left: 0;
		right: 0;
		height: 2px;
		background: oklch(from var(--color-base-content) l c h / 0.08);
	}
	.dn-dock__rail-fill {
		height: 100%;
		/* rides the record's own color when the tint sampler has one */
		background: rgb(var(--dn-now-tint, 200 220 171) / 0.95);
		box-shadow: 0 0 10px rgb(var(--dn-now-tint, 200 220 171) / 0.55);
		transition:
			width 0.3s linear,
			background 0.6s ease;
	}

	.dn-dock__capsule {
		display: flex;
		align-items: center;
		gap: 0.65rem;
		height: 100%;
		padding: 0 0.75rem;
		cursor: pointer;
	}
	/* the scoped rule above outweighs Tailwind's sm:hidden utility, so the
	   breakpoint swap is repeated here */
	@media (min-width: 640px) {
		.dn-dock__capsule {
			display: none;
		}
	}
</style>
