<script lang="ts">
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
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { getLyricsQuery } from '$lib/queries/lyrics/LyricsQuery.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { navidromeFolderScope } from '$lib/queries/remotes/RemoteQueries.svelte';
	import {
		X,
		Music,
		Shuffle,
		SkipBack,
		CircleAlert,
		Pause,
		Play,
		LoaderCircle,
		SkipForward,
		Volume2,
		ExternalLink,
		Check,
		CircleX,
		ListMusic,
		SlidersHorizontal,
		Music2
	} from 'lucide-svelte';

	let eqPanelOpen = $state(false);
	let queueDrawerOpen = $state(false);

	let lyricsPanelOpen = $state(false);

	const lyricsQuery = getLyricsQuery(
		() => playerStore.nowPlaying,
		() => authStore.user?.id,
		() => navidromeFolderScope(authStore.user?.id)
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

	const isYouTube = $derived(playerStore.nowPlaying?.sourceType === 'youtube');

	const playPauseIcon = $derived(
		playerStore.playbackState === 'error'
			? CircleAlert
			: playerStore.isBuffering
				? LoaderCircle
				: playerStore.isPlaying
					? Pause
					: Play
	);
</script>

{#if playerStore.isPlayerVisible && playerStore.nowPlaying && !deckFocus.inView}
	<div
		class="droppedneedle-player-bar fixed left-0 right-0 z-50 bg-base-300/95 backdrop-blur-md shadow-[0_-4px_20px_rgba(0,0,0,0.3)] transition-transform duration-300"
	>
		<IconButton
			size="xs"
			icon={X}
			label="Close player"
			class="absolute top-1.5 right-1.5"
			onclick={() => playerStore.stop()}
		/>

		<div
			class="droppedneedle-player-inner flex items-center gap-2 px-3 pr-9 sm:gap-4 sm:px-4 sm:pr-10 max-w-screen-2xl mx-auto"
		>
			<div class="flex min-w-0 flex-1 items-center gap-2 sm:gap-3 lg:w-1/4 lg:flex-none">
				<AlbumImage
					mbid={playerStore.nowPlaying.albumId}
					customUrl={playerStore.nowPlaying.coverUrl ?? null}
					alt={playerStore.nowPlaying.albumName}
					size="full"
					requestSize={250}
					lazy={false}
					className="w-12 h-12 sm:w-15 sm:h-15 shadow-lg ring-1 ring-base-content/10"
				/>
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
									href={withBasePath(`/album/${playerStore.nowPlaying.albumId}`)}
									class="hover:underline">{playerStore.nowPlaying.albumName}</a
								>
							{:else}
								{playerStore.nowPlaying.albumName}
							{/if}
							-
							{#if playerStore.nowPlaying.artistId}
								<a
									href={withBasePath(`/artist/${playerStore.nowPlaying.artistId}`)}
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
									href={withBasePath(`/album/${playerStore.nowPlaying.albumId}`)}
									class="hover:underline">{playerStore.nowPlaying.albumName}</a
								>
							{:else}
								{playerStore.nowPlaying.albumName}
							{/if}
						</p>
						<p class="text-xs opacity-60 truncate">
							{#if playerStore.nowPlaying.artistId}
								<a
									href={withBasePath(`/artist/${playerStore.nowPlaying.artistId}`)}
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
						<IconButton
							size="sm"
							icon={Shuffle}
							label="Toggle shuffle"
							active={playerStore.shuffleEnabled}
							aria-pressed={playerStore.shuffleEnabled}
							class="hidden sm:inline-flex"
							onclick={() => playerStore.toggleShuffle()}
						/>
					{/if}

					<IconButton
						size="sm"
						icon={SkipBack}
						label="Previous"
						class="[&_svg]:fill-current"
						disabled={!playerStore.hasPrevious}
						onclick={() => playerStore.previousTrack()}
					/>

					<!-- Buffering shows a spinning icon rather than `loading`, which would block a pause. -->
					<IconButton
						variant="accent"
						icon={playPauseIcon}
						label={playerStore.playbackState === 'error'
							? 'Close'
							: playerStore.isPlaying
								? 'Pause'
								: 'Play'}
						class={playerStore.playbackState === 'error'
							? ''
							: playerStore.isBuffering
								? 'motion-safe:[&_svg]:animate-spin'
								: '[&_svg]:fill-current'}
						onclick={() =>
							playerStore.playbackState === 'error' ? playerStore.stop() : playerStore.togglePlay()}
					/>

					<IconButton
						size="sm"
						icon={SkipForward}
						label="Next"
						class="[&_svg]:fill-current"
						disabled={!playerStore.hasNext}
						onclick={() => playerStore.nextTrack()}
					/>
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
						aria-label="Seek"
						aria-valuetext="{formatTime(playerStore.progress)} of {formatTime(
							playerStore.duration
						)}"
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
				<span class="relative inline-flex">
					<IconButton
						size="sm"
						icon={ListMusic}
						label="Toggle queue"
						active={queueDrawerOpen}
						aria-pressed={queueDrawerOpen}
						onclick={() => (queueDrawerOpen = !queueDrawerOpen)}
					/>
					{#if playerStore.upcomingQueueLength > 0}
						<span
							class="pointer-events-none badge badge-xs badge-accent absolute -top-1 -right-1"
							aria-label="{playerStore.upcomingQueueLength} upcoming"
							>{playerStore.upcomingQueueLength}</span
						>
					{/if}
				</span>

				{#if supportsLyrics}
					<IconButton
						size="sm"
						icon={Music2}
						label="Toggle lyrics"
						active={lyricsPanelOpen}
						aria-pressed={lyricsPanelOpen}
						onclick={toggleLyrics}
					/>
				{/if}

				<IconButton
					size="sm"
					icon={SlidersHorizontal}
					label={isYouTube ? 'Equalizer (unavailable for YouTube)' : 'Toggle equalizer'}
					active={eqStore.enabled && !isYouTube}
					onclick={() => (eqPanelOpen = !eqPanelOpen)}
				/>

				<div class="hidden sm:flex items-center gap-1.5">
					<Volume2 class="h-4 w-4 opacity-60 shrink-0" aria-hidden="true" />
					<input
						type="range"
						class="range range-xs w-20"
						min="0"
						max="100"
						value={playerStore.volume}
						oninput={handleVolume}
						aria-label="Volume"
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

					<IconButton
						size="sm"
						icon={ExternalLink}
						label="Open in YouTube"
						onclick={openInYouTube}
					/>
				{:else if playerStore.nowPlaying.sourceType === 'jellyfin'}
					<div class="hidden sm:flex items-center gap-2 text-[rgb(var(--brand-jellyfin))]">
						<JellyfinIcon class="h-5 w-5" />
						<span class="text-sm font-medium">Jellyfin</span>
					</div>
				{:else if playerStore.nowPlaying.sourceType === 'navidrome'}
					<div class="hidden sm:flex items-center gap-2 text-[rgb(var(--brand-navidrome))]">
						<NavidromeIcon class="h-5 w-5" />
						<span class="text-sm font-medium">Navidrome</span>
					</div>
				{:else if playerStore.nowPlaying.sourceType === 'plex'}
					<div class="hidden sm:flex items-center gap-2 text-[rgb(var(--brand-plex))]">
						<PlexIcon class="h-5 w-5" />
						<span class="text-sm font-medium">Plex</span>
					</div>
				{:else if playerStore.nowPlaying.sourceType === 'local'}
					<div class="hidden sm:flex items-center gap-2 text-[rgb(var(--brand-localfiles))]">
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

<style>
	.droppedneedle-player-bar {
		bottom: var(--ms-bottom-nav-offset);
		height: var(--ms-player-height);
	}

	.droppedneedle-player-inner {
		height: 100%;
	}

	@media (min-width: 768px) {
		.droppedneedle-player-bar {
			bottom: 0;
		}
	}
</style>
