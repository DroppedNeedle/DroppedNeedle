<script lang="ts">
	import { resolve } from '$app/paths';
	import { playerStore } from '$lib/stores/player.svelte';
	import { deckFocus } from '$lib/stores/deckFocus.svelte';
	import { eqStore } from '$lib/stores/eq.svelte';
	import { scrobbleManager } from '$lib/stores/scrobble.svelte';
	import YouTubePlayer from '$lib/components/YouTubePlayer.svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import QueueDrawer from '$lib/components/QueueDrawer.svelte';
	import EqPanel from '$lib/components/EqPanel.svelte';
	import LyricsPanel from '$lib/components/LyricsPanel.svelte';
	import AudioQualityBadge from '$lib/components/AudioQualityBadge.svelte';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { getLyricsQuery } from '$lib/queries/lyrics/LyricsQuery.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getNavidromeFolderScopeRevision } from '$lib/utils/navidromeLibraryCache';
	import {
		X,
		Music,
		Disc3,
		Shuffle,
		SkipBack,
		AlertCircle,
		Pause,
		Play,
		SkipForward,
		Volume2,
		ExternalLink,
		Check,
		CircleX,
		ListMusic,
		SlidersHorizontal,
		Music2
	} from 'lucide-svelte';

	let coverImgError = $state(false);
	let lastCoverKey = '';
	let eqPanelOpen = $state(false);
	let queueDrawerOpen = $state(false);

	let lyricsPanelOpen = $state(false);

	const lyricsQuery = getLyricsQuery(
		() => playerStore.nowPlaying,
		() => authStore.user?.id,
		() => getNavidromeFolderScopeRevision(authStore.user?.id ?? '')
	);

	const supportsLyrics = $derived(lyricsQuery.isSuccess && lyricsQuery.data !== null);

	$effect(() => {
		void playerStore.nowPlaying;
		lyricsPanelOpen = false;
	});

	function toggleLyrics() {
		lyricsPanelOpen = !lyricsPanelOpen;
	}

	function formatTime(seconds: number): string {
		if (!seconds || isNaN(seconds)) return '0:00';
		const mins = Math.floor(seconds / 60);
		const secs = Math.floor(seconds % 60);
		return `${mins}:${secs.toString().padStart(2, '0')}`;
	}

	function handleSeek(e: Event): void {
		const target = e.target as HTMLInputElement;
		playerStore.seekTo(Number(target.value));
	}

	function handleVolume(e: Event): void {
		const target = e.target as HTMLInputElement;
		playerStore.setVolume(Number(target.value));
	}

	const MBID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

	function isAlbumLinkable(id: string | undefined): boolean {
		return !!id && MBID_RE.test(id);
	}

	function openInYouTube(): void {
		const trackSourceId = playerStore.nowPlaying?.trackSourceId;
		if (trackSourceId) {
			window.open(`https://www.youtube.com/watch?v=${trackSourceId}`, '_blank');
		}
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
</script>

{#if playerStore.isPlayerVisible && playerStore.nowPlaying && !deckFocus.inView}
	<div
		class="droppedneedle-player-bar fixed left-0 right-0 z-50 bg-base-300/95 backdrop-blur-md shadow-[0_-4px_20px_rgba(0,0,0,0.3)] transition-transform duration-300"
	>
		<button
			class="btn btn-ghost btn-xs btn-circle absolute top-1.5 right-1.5 opacity-60 hover:opacity-100"
			onclick={() => playerStore.stop()}
			aria-label="Close player"
		>
			<X class="h-3.5 w-3.5" />
		</button>

		<div
			class="droppedneedle-player-inner flex items-center gap-2 px-3 pr-9 sm:gap-4 sm:px-4 sm:pr-10 max-w-screen-2xl mx-auto"
		>
			<div class="flex min-w-0 flex-1 items-center gap-2 sm:gap-3 lg:w-1/4 lg:flex-none">
				{#if nowPlayingCoverUrl && !coverImgError}
					<img
						src={nowPlayingCoverUrl}
						alt={playerStore.nowPlaying.albumName}
						class="w-12 h-12 sm:w-15 sm:h-15 rounded-lg shadow-lg ring-1 ring-base-content/10 object-cover shrink-0"
						onerror={() => {
							coverImgError = true;
						}}
					/>
				{:else}
					<div
						class="w-12 h-12 sm:w-15 sm:h-15 rounded-lg shadow-lg bg-base-200 flex items-center justify-center shrink-0"
					>
						<Disc3 class="h-6 w-6 text-base-content/20" />
					</div>
				{/if}
				{#if playerStore.isPlaying}
					<div class="hidden sm:block">
						<NowPlayingIndicator size="md" />
					</div>
				{/if}
				<div class="min-w-0 pr-1">
					{#if playerStore.nowPlaying.trackName}
						<p class="text-sm font-semibold truncate">{playerStore.nowPlaying.trackName}</p>
						<p class="text-xs opacity-60 truncate">
							{#if isAlbumLinkable(playerStore.nowPlaying.albumId)}
								<a
									href={resolve(`/album/${playerStore.nowPlaying.albumId}`)}
									class="hover:underline">{playerStore.nowPlaying.albumName}</a
								>
							{:else}
								{playerStore.nowPlaying.albumName}
							{/if}
							-
							{#if playerStore.nowPlaying.artistId}
								<a
									href={resolve(`/artist/${playerStore.nowPlaying.artistId}`)}
									class="hover:underline">{playerStore.nowPlaying.artistName}</a
								>
							{:else}
								{playerStore.nowPlaying.artistName}
							{/if}
						</p>
					{:else}
						<p class="text-sm font-semibold truncate">
							{#if isAlbumLinkable(playerStore.nowPlaying.albumId)}
								<a
									href={resolve(`/album/${playerStore.nowPlaying.albumId}`)}
									class="hover:underline">{playerStore.nowPlaying.albumName}</a
								>
							{:else}
								{playerStore.nowPlaying.albumName}
							{/if}
						</p>
						<p class="text-xs opacity-60 truncate">
							{#if playerStore.nowPlaying.artistId}
								<a
									href={resolve(`/artist/${playerStore.nowPlaying.artistId}`)}
									class="hover:underline">{playerStore.nowPlaying.artistName}</a
								>
							{:else}
								{playerStore.nowPlaying.artistName}
							{/if}
						</p>
					{/if}
					{#if playerStore.hasQueue}
						<p class="text-xs opacity-40 truncate">
							Track {playerStore.currentTrackNumber} of {playerStore.queueLength}
						</p>
					{/if}
					{#if playerStore.nowPlaying.format}
						<AudioQualityBadge codec={playerStore.nowPlaying.format} compact />
					{/if}
					{#if playerStore.playbackState === 'error'}
						<p class="text-xs text-error truncate">This track isn't available right now.</p>
					{/if}
				</div>
			</div>

			<div class="flex shrink-0 flex-col items-center justify-center gap-1 sm:flex-1">
				<div class="flex items-center gap-1 sm:gap-3">
					{#if playerStore.hasQueue}
						<button
							class="btn btn-ghost btn-sm btn-circle hidden sm:inline-flex"
							class:text-accent={playerStore.shuffleEnabled}
							class:opacity-50={!playerStore.shuffleEnabled}
							onclick={() => playerStore.toggleShuffle()}
							aria-label="Toggle shuffle"
						>
							<Shuffle class="h-4 w-4" />
						</button>
					{/if}

					<button
						class="btn btn-ghost btn-sm btn-circle"
						class:opacity-30={!playerStore.hasPrevious}
						class:cursor-not-allowed={!playerStore.hasPrevious}
						disabled={!playerStore.hasPrevious}
						onclick={() => playerStore.previousTrack()}
						aria-label="Previous"
					>
						<SkipBack class="h-4 w-4 fill-current" />
					</button>

					<button
						class="btn btn-circle btn-accent shadow-md w-10 h-10"
						onclick={() =>
							playerStore.playbackState === 'error' ? playerStore.stop() : playerStore.togglePlay()}
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
							<Play class="h-5 w-5 ml-0.5 fill-current" />
						{/if}
					</button>

					<button
						class="btn btn-ghost btn-sm btn-circle"
						class:opacity-30={!playerStore.hasNext}
						class:cursor-not-allowed={!playerStore.hasNext}
						disabled={!playerStore.hasNext}
						onclick={() => playerStore.nextTrack()}
						aria-label="Next"
					>
						<SkipForward class="h-4 w-4 fill-current" />
					</button>
				</div>

				<div class="hidden sm:flex items-center gap-2 w-full max-w-lg">
					<span class="text-xs opacity-60 w-10 text-right tabular-nums"
						>{formatTime(playerStore.progress)}</span
					>
					<input
						type="range"
						class="range range-xs range-accent flex-1"
						class:opacity-50={!playerStore.isSeekable}
						class:cursor-not-allowed={!playerStore.isSeekable}
						min="0"
						max={playerStore.duration || 1}
						value={playerStore.progress}
						disabled={!playerStore.isSeekable}
						oninput={handleSeek}
					/>
					<span class="text-xs opacity-60 w-10 tabular-nums"
						>{formatTime(playerStore.duration)}</span
					>
				</div>
				{#if !playerStore.isSeekable}
					<p class="hidden sm:block text-[10px] text-base-content/60">
						This stream doesn't support seeking.
					</p>
				{/if}
			</div>

			<div class="hidden md:flex items-center gap-3 lg:gap-7 lg:w-1/4 justify-end">
				<div class="tooltip tooltip-left" data-tip="Queue">
					<button
						class="btn btn-ghost btn-sm btn-circle relative"
						class:text-accent={queueDrawerOpen}
						onclick={() => (queueDrawerOpen = !queueDrawerOpen)}
						aria-label="Toggle queue"
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
					<div class="tooltip tooltip-left" data-tip="Lyrics">
						<button
							class="btn btn-ghost btn-sm btn-circle"
							class:text-accent={lyricsPanelOpen}
							onclick={toggleLyrics}
							aria-label="Toggle lyrics"
						>
							<Music2 class="h-4 w-4" />
						</button>
					</div>
				{/if}

				<div
					class="tooltip tooltip-left"
					data-tip={playerStore.nowPlaying?.sourceType === 'youtube'
						? 'EQ unavailable for YouTube'
						: 'Equalizer'}
				>
					<button
						class="btn btn-ghost btn-sm btn-circle"
						class:text-accent={eqStore.enabled && playerStore.nowPlaying?.sourceType !== 'youtube'}
						class:opacity-30={playerStore.nowPlaying?.sourceType === 'youtube'}
						onclick={() => (eqPanelOpen = !eqPanelOpen)}
						aria-label="Toggle equalizer"
					>
						<SlidersHorizontal class="h-4 w-4" />
					</button>
				</div>

				<div class="hidden sm:flex items-center gap-1.5">
					<Volume2 class="h-4 w-4 opacity-60 shrink-0" />
					<input
						type="range"
						class="range range-xs w-20"
						min="0"
						max="100"
						value={playerStore.volume}
						oninput={handleVolume}
					/>
				</div>

				{#if scrobbleManager.enabled && scrobbleManager.status !== 'idle'}
					<div class="tooltip tooltip-left" data-tip={scrobbleManager.tooltip}>
						{#if scrobbleManager.status === 'scrobbled'}
							<Check class="h-4 w-4 text-success" />
						{:else if scrobbleManager.status === 'error'}
							<CircleX class="h-4 w-4 text-error" />
						{:else}
							<span class="badge badge-info badge-sm gap-1 font-semibold">
								<span class="status status-md status-info"></span>
								Tracking
							</span>
						{/if}
					</div>
				{/if}

				{#if playerStore.nowPlaying.sourceType === 'youtube'}
					<YouTubePlayer />

					<div class="tooltip tooltip-left" data-tip="Open in YouTube">
						<button
							class="btn btn-ghost btn-sm btn-circle"
							onclick={openInYouTube}
							aria-label="Open in YouTube"
						>
							<ExternalLink class="h-4 w-4" />
						</button>
					</div>
				{:else if playerStore.nowPlaying.sourceType === 'jellyfin'}
					<div class="hidden sm:flex items-center gap-2" style="color: rgb(var(--brand-jellyfin))">
						<JellyfinIcon class="h-5 w-5" />
						<span class="text-sm font-medium">Jellyfin</span>
					</div>
				{:else if playerStore.nowPlaying.sourceType === 'navidrome'}
					<div class="hidden sm:flex items-center gap-2" style="color: rgb(var(--brand-navidrome))">
						<NavidromeIcon class="h-5 w-5" />
						<span class="text-sm font-medium">Navidrome</span>
					</div>
				{:else if playerStore.nowPlaying.sourceType === 'plex'}
					<div class="hidden sm:flex items-center gap-2" style="color: rgb(var(--brand-plex))">
						<PlexIcon class="h-5 w-5" />
						<span class="text-sm font-medium">Plex</span>
					</div>
				{:else if playerStore.nowPlaying.sourceType === 'local'}
					<div
						class="hidden sm:flex items-center gap-2"
						style="color: rgb(var(--brand-localfiles))"
					>
						<Music class="h-5 w-5" />
						<span class="text-sm font-medium"
							>Local{#if playerStore.currentQueueItem?.format}<span
									class="badge badge-xs badge-ghost ml-1 uppercase"
									>{playerStore.currentQueueItem.format}</span
								>{/if}</span
						>
					</div>
				{/if}
			</div>
		</div>
	</div>

	<QueueDrawer bind:open={queueDrawerOpen} onclose={() => (queueDrawerOpen = false)} />
	<EqPanel bind:open={eqPanelOpen} onclose={() => (eqPanelOpen = false)} />
	<LyricsPanel
		bind:open={lyricsPanelOpen}
		lyricsText={lyricsQuery.data?.text ?? ''}
		lines={lyricsQuery.data?.lines ?? []}
		isSynced={lyricsQuery.data?.is_synced ?? false}
		isLoading={lyricsQuery.isFetching}
		hasError={lyricsQuery.isError}
		currentTime={playerStore.progress}
		trackName={playerStore.nowPlaying?.trackName ?? ''}
		artistName={playerStore.nowPlaying?.artistName ?? ''}
		onclose={() => (lyricsPanelOpen = false)}
	/>
{/if}
