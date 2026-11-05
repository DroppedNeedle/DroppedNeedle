<!--
	The player bar: a bordered panel along the bottom of every signed-in page
	while a track is loaded. Left, what is playing; middle, the transport and
	the seek bar; right (medium width and up), the buttons that open the
	drawer on its Queue, Lyrics and Equalizer tabs, the volume and where the
	track comes from. "Close player" is a tab on the bar's top edge. Phones
	get the cover, the names and previous, play and next.
-->
<script lang="ts">
	import { isValidMbid } from '$lib/utils/formatting';
	import { formatLabel, playerStore } from '$lib/features/player/state/player.svelte';
	import { deckFocus } from '$lib/features/player/state/deckFocus.svelte';
	import { eqStore } from '$lib/features/player/state/eq.svelte';
	import { scrobbleManager } from '$lib/features/player/state/scrobble.svelte';
	import {
		playerDrawer,
		type PlayerDrawerTab
	} from '$lib/features/player/state/playerDrawer.svelte';
	import YouTubePlayer from '$lib/features/player/components/YouTubePlayer.svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import YouTubeIcon from '$lib/media/icons/YouTubeIcon.svelte';
	import PlayerDrawer from '$lib/features/player/components/PlayerDrawer.svelte';
	import AudioQualityBadge from '$lib/features/player/components/AudioQualityBadge.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import PlayerTrackMarks from '$lib/features/player/components/PlayerTrackMarks.svelte';
	import StarPicker from '$lib/features/player/components/StarPicker.svelte';
	import { libraryTrackId, skipByRating } from '$lib/features/player/state/skipByRating.svelte';
	import { getRatingsQuery } from '$lib/features/library/queries/ratings.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { getLyricsQuery } from '$lib/features/player/queries/lyrics.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { navidromeFolderScope } from '$lib/features/remote-library/queries/RemoteQueries.svelte';
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
		ExternalLink,
		Check,
		CircleX,
		ListMusic,
		SlidersHorizontal,
		FileText,
		FastForward
	} from 'lucide-svelte';

	// The queue's track ratings, read only while the rating floor is on and
	// kept fresh by the ratings cache (a rating given anywhere moves it).
	const queueTrackIds = $derived(
		skipByRating.minStars > 0
			? [...new Set(playerStore.queue.map(libraryTrackId).filter((id) => id !== null))].sort()
			: []
	);
	const queueRatings = getRatingsQuery(() =>
		queueTrackIds.length > 0 ? { trackIds: queueTrackIds } : null
	);
	$effect(() => {
		skipByRating.ratings = queueTrackIds.length > 0 ? (queueRatings.data?.tracks ?? null) : null;
	});

	const lyricsQuery = getLyricsQuery(
		() => playerStore.nowPlaying,
		() => authStore.user?.id,
		() => navidromeFolderScope(authStore.user?.id)
	);

	const supportsLyrics = $derived(lyricsQuery.isSuccess && lyricsQuery.data !== null);
	const barShowing = $derived(
		playerStore.isPlayerVisible && !!playerStore.nowPlaying && !deckFocus.inView
	);
	const shownTab = $derived<PlayerDrawerTab>(
		playerDrawer.tab === 'lyrics' && !supportsLyrics ? 'queue' : playerDrawer.tab
	);

	// The drawer hangs off the bar, so it goes when the bar does.
	$effect(() => {
		if (!barShowing) playerDrawer.open = false;
	});

	function isOpenOn(tab: PlayerDrawerTab): boolean {
		return playerDrawer.open && shownTab === tab;
	}

	// A bar button opens the drawer on its tab, or shuts it when that tab is already showing.
	function toggleTab(tab: PlayerDrawerTab): void {
		if (isOpenOn(tab)) {
			playerDrawer.open = false;
			return;
		}
		playerDrawer.tab = tab;
		playerDrawer.open = true;
	}

	function formatTime(seconds: number): string {
		if (!seconds || isNaN(seconds)) return '0:00';
		const mins = Math.floor(seconds / 60);
		const secs = Math.floor(seconds % 60);
		return `${mins}:${secs.toString().padStart(2, '0')}`;
	}

	function handleSeek(e: Event): void {
		playerStore.seekTo(Number((e.target as HTMLInputElement).value));
	}

	function handleVolume(e: Event): void {
		playerStore.setVolume(Number((e.target as HTMLInputElement).value));
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
	const isError = $derived(playerStore.playbackState === 'error');
	const unplayableReason = $derived(
		isError && playerStore.currentQueueItem
			? playerStore.unplayableReasonFor(playerStore.currentQueueItem)
			: null
	);
	const unplayableNote = $derived.by(() => {
		const format = formatLabel(playerStore.currentQueueItem?.format);
		switch (unplayableReason) {
			case 'converting_off':
				return `This browser can't play ${format}, and transcoding is off.`;
			case 'cannot_convert':
				return `This browser can't play ${format}, and this server can't convert it.`;
			case 'conversion_failed':
				return "This track couldn't be converted for this browser.";
			default:
				return "This track isn't available right now.";
		}
	});
	// What fits beside the cover on a phone; the full note is its accessible text.
	const unplayableShort = $derived.by(() => {
		if (!unplayableReason) return null;
		if (unplayableReason === 'conversion_failed') return "Couldn't convert it";
		return `Can't play ${formatLabel(playerStore.currentQueueItem?.format)} here`;
	});
	const progressPct = $derived(
		playerStore.duration > 0
			? Math.min(100, (playerStore.progress / playerStore.duration) * 100)
			: 0
	);

	const playPauseIcon = $derived(
		isError
			? CircleAlert
			: playerStore.isBuffering
				? LoaderCircle
				: playerStore.isPlaying
					? Pause
					: Play
	);
</script>

{#snippet albumName()}
	{#if isAlbumLinkable(playerStore.nowPlaying?.albumId)}
		<a href={withBasePath(`/album/${playerStore.nowPlaying?.albumId}`)} class="dn-player__link"
			>{playerStore.nowPlaying?.albumName}</a
		>
	{:else}
		{playerStore.nowPlaying?.albumName}
	{/if}
{/snippet}

{#snippet artistName()}
	{#if playerStore.nowPlaying?.artistId}
		<a href={withBasePath(`/artist/${playerStore.nowPlaying.artistId}`)} class="dn-player__link"
			>{playerStore.nowPlaying.artistName}</a
		>
	{:else}
		{playerStore.nowPlaying?.artistName}
	{/if}
{/snippet}

{#if barShowing && playerStore.nowPlaying}
	{@const np = playerStore.nowPlaying}
	<div class="droppedneedle-player-bar dn-player">
		<Button
			variant="neutral"
			size="xs"
			icon={X}
			aria-label="Close player"
			class="dn-player__close"
			onclick={() => playerStore.stop()}
		>
			<span class="hidden md:inline">Close player</span>
		</Button>

		<span class="dn-player__phone-progress" style:--progress="{progressPct}%" aria-hidden="true"
		></span>

		<div class="dn-player__grid">
			<div class="dn-player__now">
				<AlbumImage
					mbid={isValidMbid(np.albumId) ? np.albumId : ''}
					customUrl={np.coverUrl ?? null}
					alt={np.albumName}
					size="full"
					requestSize={250}
					lazy={false}
					rounded="sm"
					className="dn-player__cover"
				/>
				<div class="min-w-0">
					<p class="truncate text-[0.9375rem] leading-tight font-bold md:text-base">
						{#if np.trackName}{np.trackName}{:else}{@render albumName()}{/if}
					</p>
					<p class="truncate text-xs text-muted md:text-[0.8125rem]">
						{#if np.trackName}{@render albumName()}<span class="px-1" aria-hidden="true">·</span
							>{/if}{@render artistName()}
					</p>
					{#if playerStore.hasQueue}
						<p class="dn-player__mono mt-0.5 hidden sm:block">
							Track {playerStore.currentTrackNumber} of {playerStore.queueLength}
						</p>
					{/if}
					{#if isError}
						<p class="line-clamp-2 text-xs font-semibold text-error" title={unplayableNote}>
							{#if unplayableShort}
								<span class="sm:hidden" aria-hidden="true">{unplayableShort}</span>
								<span class="sr-only sm:not-sr-only">{unplayableNote}</span>
							{:else}
								{unplayableNote}
							{/if}
							{#if unplayableReason === 'converting_off' && authStore.isAdmin}
								<a href={withBasePath('/settings?tab=connect-apps')} class="dn-player__link"
									>Turn on</a
								>
							{/if}
						</p>
					{:else if np.format || (scrobbleManager.enabled && scrobbleManager.status !== 'idle')}
						<div class="mt-1 hidden items-center gap-1.5 sm:flex">
							{#if np.format}
								<AudioQualityBadge codec={np.format} converted={playerStore.converting} compact />
							{/if}
							{#if scrobbleManager.enabled && scrobbleManager.status !== 'idle'}
								<span class="tooltip tooltip-top" data-tip={scrobbleManager.tooltip}>
									{#if scrobbleManager.status === 'scrobbled'}
										<StatusChip size="xs" tone="success" icon={Check}>
											<span class="sr-only">{scrobbleManager.tooltip}</span>
										</StatusChip>
									{:else if scrobbleManager.status === 'error'}
										<StatusChip size="xs" tone="error" icon={CircleX}>
											<span class="sr-only">{scrobbleManager.tooltip}</span>
										</StatusChip>
									{:else}
										<StatusChip size="xs" icon={Check}>Tracking</StatusChip>
									{/if}
								</span>
							{/if}
						</div>
					{/if}
				</div>
			</div>

			<div class="dn-player__transport">
				<div class="flex items-center gap-1 sm:gap-2.5">
					{#if playerStore.hasQueue}
						<span class="hidden sm:inline-flex">
							<StarPicker
								label={skipByRating.minStars > 0
									? `Skipping tracks rated under ${skipByRating.minStars} stars`
									: 'Skip tracks with low rating'}
								value={skipByRating.minStars}
								clear="off"
								active={skipByRating.minStars > 0}
								onpick={(stars) => skipByRating.set(stars)}
							>
								<span class="relative inline-flex size-5 items-center justify-center">
									<FastForward class="size-5" aria-hidden="true" />
									{#if skipByRating.minStars > 0}
										<span class="badge badge-warning badge-xs absolute -right-2 -top-2 px-1"
											>{skipByRating.minStars}★</span
										>
									{/if}
								</span>
							</StarPicker>
						</span>
						<IconButton
							icon={Shuffle}
							label="Toggle shuffle"
							active={playerStore.shuffleEnabled}
							aria-pressed={playerStore.shuffleEnabled}
							class="hidden sm:inline-flex"
							onclick={() => playerStore.toggleShuffle()}
						/>
					{/if}
					<IconButton
						icon={SkipBack}
						label="Previous"
						class="[&_svg]:fill-current"
						disabled={!playerStore.hasPrevious}
						onclick={() => playerStore.previousTrack()}
					/>
					<!-- Buffering shows a spinning icon rather than `loading`, which would block a pause. -->
					<IconButton
						variant="primary"
						size="lg"
						icon={playPauseIcon}
						label={isError ? 'Close' : playerStore.isPlaying ? 'Pause' : 'Play'}
						class={isError
							? ''
							: playerStore.isBuffering
								? 'motion-safe:[&_svg]:animate-spin'
								: '[&_svg]:fill-current'}
						onclick={() => (isError ? playerStore.stop() : playerStore.togglePlay())}
					/>
					<IconButton
						icon={SkipForward}
						label="Next"
						class="[&_svg]:fill-current"
						disabled={!playerStore.hasNext}
						onclick={() => playerStore.nextTrack()}
					/>

					<PlayerTrackMarks />
				</div>

				<div class="hidden w-full items-center gap-2.5 sm:flex">
					<span class="dn-player__time text-right">{formatTime(playerStore.progress)}</span>
					<input
						type="range"
						class="dn-range min-w-0 flex-1"
						style:--fill="{progressPct}%"
						min="0"
						max={playerStore.duration || 1}
						step="any"
						value={playerStore.progress}
						disabled={!playerStore.isSeekable}
						oninput={handleSeek}
						aria-label="Seek"
						aria-valuetext="{formatTime(playerStore.progress)} of {formatTime(
							playerStore.duration
						)}"
					/>
					<span class="dn-player__time">{formatTime(playerStore.duration)}</span>
				</div>
				{#if !playerStore.isSeekable}
					<p class="hidden text-[0.6875rem] text-muted sm:block">
						This stream doesn't support seeking.
					</p>
				{/if}
			</div>

			<div class="dn-player__tools">
				<div class="flex items-center gap-2">
					<span class="relative inline-flex">
						<IconButton
							variant={isOpenOn('queue') ? 'primary' : 'neutral'}
							shape="square"
							icon={ListMusic}
							label="Toggle queue"
							aria-pressed={isOpenOn('queue')}
							onclick={() => toggleTab('queue')}
						/>
						{#if playerStore.upcomingQueueLength > 0}
							<span
								class="dn-player__count"
								class:dn-player__count--on={isOpenOn('queue')}
								aria-label="{playerStore.upcomingQueueLength} upcoming"
								>{playerStore.upcomingQueueLength}</span
							>
						{/if}
					</span>
					{#if supportsLyrics}
						<IconButton
							variant={isOpenOn('lyrics') ? 'primary' : 'neutral'}
							shape="square"
							icon={FileText}
							label="Toggle lyrics"
							aria-pressed={isOpenOn('lyrics')}
							onclick={() => toggleTab('lyrics')}
						/>
					{/if}
					<span class="relative inline-flex">
						<IconButton
							variant={isOpenOn('eq') ? 'primary' : 'neutral'}
							shape="square"
							icon={SlidersHorizontal}
							label={isYouTube ? 'Equalizer (unavailable for YouTube)' : 'Toggle equalizer'}
							aria-pressed={isOpenOn('eq')}
							onclick={() => toggleTab('eq')}
						/>
						{#if eqStore.enabled && !isYouTube && !isOpenOn('eq')}
							<span class="dn-player__dot" aria-hidden="true"></span>
						{/if}
					</span>
				</div>

				<span class="dn-player__rule"></span>

				<div class="flex flex-col gap-1">
					<span class="hidden items-baseline justify-between lg:flex" aria-hidden="true">
						<span class="text-xs font-semibold">Volume</span>
						<span class="dn-player__mono">{playerStore.volume}</span>
					</span>
					<input
						type="range"
						class="dn-range w-20 lg:w-28"
						style:--fill="{playerStore.volume}%"
						min="0"
						max="100"
						value={playerStore.volume}
						oninput={handleVolume}
						aria-label="Volume"
					/>
				</div>

				<span class="dn-player__rule"></span>

				{#if np.sourceType === 'youtube'}
					<span class="dn-player__source text-[var(--color-youtube)]">
						<YouTubeIcon class="size-4" />
						<span class="dn-player__source-name">YouTube</span>
					</span>
					<IconButton
						size="sm"
						icon={ExternalLink}
						label="Open in YouTube"
						onclick={openInYouTube}
					/>
				{:else if np.sourceType === 'jellyfin'}
					<span class="dn-player__source text-jellyfin">
						<JellyfinIcon class="size-4" />
						<span class="dn-player__source-name">Jellyfin</span>
					</span>
				{:else if np.sourceType === 'navidrome'}
					<span class="dn-player__source text-navidrome">
						<NavidromeIcon class="size-4" />
						<span class="dn-player__source-name">Navidrome</span>
					</span>
				{:else if np.sourceType === 'plex'}
					<span class="dn-player__source text-plex">
						<PlexIcon class="size-4" />
						<span class="dn-player__source-name">Plex</span>
					</span>
				{:else if np.sourceType === 'local'}
					<span class="dn-player__source text-localfiles">
						<Music class="size-4" aria-hidden="true" />
						<span class="dn-player__source-name">Local</span>
						{#if playerStore.currentQueueItem?.format}
							<span class="dn-player__source-name" aria-hidden="true">·</span>
							<span class="dn-player__source-name font-mono uppercase"
								>{playerStore.currentQueueItem.format}</span
							>
						{/if}
					</span>
				{/if}
			</div>
		</div>
	</div>

	{#if isYouTube}
		<YouTubePlayer />
	{/if}

	<PlayerDrawer
		bind:open={playerDrawer.open}
		bind:tab={playerDrawer.tab}
		onclose={() => (playerDrawer.open = false)}
	/>
{/if}

<style>
	/* Pinned above the phone's bottom bar; a little in from the edges. The
	   bar fills --ms-player-height less its inset, so the page, toasts and
	   the drawer all keep clear of it. */
	.dn-player {
		position: fixed;
		z-index: 56;
		left: 0.5rem;
		right: 0.5rem;
		bottom: calc(var(--ms-bottom-nav-offset) + 0.375rem);
		height: calc(var(--ms-player-height) - 0.75rem);
		border: 1px solid var(--dn-line);
		border-radius: var(--radius-box);
		background: var(--color-base-200);
		box-shadow: 0 -10px 36px -12px rgb(0 0 0 / 0.55);
	}

	.dn-player :global(.dn-player__close) {
		position: absolute;
		right: 1rem;
		bottom: 100%;
		border-bottom-width: 0;
		border-bottom-right-radius: 0;
		border-bottom-left-radius: 0;
	}

	.dn-player__grid {
		display: grid;
		height: 100%;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.75rem;
		padding: 0 0.5rem 0 0.5rem;
	}

	.dn-player__now {
		display: flex;
		min-width: 0;
		align-items: center;
		gap: 0.75rem;
	}
	.dn-player__now :global(.dn-player__cover) {
		width: 3.25rem;
		height: 3.25rem;
		flex-shrink: 0;
	}

	.dn-player__link:hover {
		text-decoration: underline;
	}

	.dn-player__mono {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	.dn-player__transport {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.25rem;
	}

	.dn-player__time {
		width: 2.75rem;
		flex-shrink: 0;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-variant-numeric: tabular-nums;
		color: var(--dn-muted);
	}

	.dn-player__tools {
		display: none;
	}

	.dn-player__rule {
		width: 1px;
		height: 2.5rem;
		background: var(--dn-line);
	}

	.dn-player__count {
		position: absolute;
		top: -0.4375rem;
		right: -0.4375rem;
		display: grid;
		min-width: 1.25rem;
		height: 1.25rem;
		place-items: center;
		padding: 0 0.3125rem;
		border: 1px solid var(--dn-action-edge);
		border-radius: 999px;
		background: var(--dn-action);
		color: var(--dn-action-content);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		pointer-events: none;
	}
	.dn-player__count--on {
		border-color: var(--dn-title);
		background: var(--color-base-200);
		color: var(--dn-title);
	}

	/* the equalizer is on */
	.dn-player__dot {
		position: absolute;
		top: 0.375rem;
		right: 0.375rem;
		width: 0.4375rem;
		height: 0.4375rem;
		border-radius: 999px;
		background: var(--dn-title);
		pointer-events: none;
	}

	.dn-player__source {
		display: inline-flex;
		height: 2rem;
		align-items: center;
		gap: 0.375rem;
		padding: 0 0.75rem;
		border: 1px solid var(--dn-edge);
		border-radius: 999px;
		font-size: 0.8125rem;
		font-weight: 600;
		white-space: nowrap;
	}

	/* phones show how far along the track is as a line on the bar's top edge */
	.dn-player__phone-progress {
		position: absolute;
		top: -1px;
		left: var(--radius-box);
		right: var(--radius-box);
		height: 2px;
		background: linear-gradient(
			to right,
			var(--dn-title) var(--progress),
			transparent var(--progress)
		);
	}

	/* the seek and volume bars: a thin rail, filled to the value, with the action thumb */
	.dn-range {
		--fill: 0%;
		height: 1.25rem;
		appearance: none;
		background: transparent;
		cursor: pointer;
	}
	.dn-range::-webkit-slider-runnable-track {
		height: 4px;
		border-radius: 2px;
		background: linear-gradient(to right, var(--dn-title) var(--fill), var(--dn-line) var(--fill));
	}
	.dn-range::-moz-range-track {
		height: 4px;
		border-radius: 2px;
		background: var(--dn-line);
	}
	.dn-range::-moz-range-progress {
		height: 4px;
		border-radius: 2px;
		background: var(--dn-title);
	}
	.dn-range::-webkit-slider-thumb {
		width: 14px;
		height: 14px;
		margin-top: -5px;
		appearance: none;
		border: 2px solid var(--dn-action-edge);
		border-radius: 999px;
		background: var(--dn-action);
		box-shadow: 0 0 0 3px var(--color-base-200);
	}
	.dn-range::-moz-range-thumb {
		width: 10px;
		height: 10px;
		border: 2px solid var(--dn-action-edge);
		border-radius: 999px;
		background: var(--dn-action);
		box-shadow: 0 0 0 3px var(--color-base-200);
	}
	.dn-range:disabled {
		cursor: not-allowed;
		opacity: 0.5;
	}
	@media (forced-colors: active) {
		.dn-range {
			forced-color-adjust: none;
		}
		.dn-range::-webkit-slider-runnable-track {
			background: linear-gradient(to right, Highlight var(--fill), CanvasText var(--fill));
		}
		.dn-range::-webkit-slider-thumb {
			border-color: Canvas;
			background: Highlight;
		}
	}
	.dn-player__source-name {
		color: var(--color-base-content);
	}

	@media (min-width: 640px) {
		.dn-player__grid {
			gap: 1.25rem;
			padding: 0 0.75rem;
		}
		.dn-player__transport {
			width: clamp(15rem, 34vw, 30rem);
		}
		.dn-player__phone-progress {
			display: none;
		}
	}

	@media (min-width: 768px) {
		.dn-player {
			left: 0.75rem;
			right: 0.75rem;
			bottom: 0.75rem;
			height: calc(var(--ms-player-height) - 1.5rem);
		}
		.dn-player__grid {
			grid-template-columns: minmax(0, 1fr) auto auto;
			padding: 0 1rem 0 0.4375rem;
		}
		.dn-player__now :global(.dn-player__cover) {
			width: 4.5rem;
			height: 4.5rem;
		}
		.dn-player__transport {
			width: clamp(12rem, 26vw, 30rem);
		}
		.dn-player__tools {
			display: flex;
			align-items: center;
			justify-content: flex-end;
			gap: 0.75rem;
		}
	}

	/* Up to the large width the tools keep to their controls: the source
	   shows its mark alone (its name stays for screen readers), no rules. */
	@media (min-width: 768px) and (max-width: 1023.98px) {
		.dn-player__rule {
			display: none;
		}
		.dn-player__source {
			padding: 0 0.5rem;
		}
		.dn-player__source-name {
			position: absolute;
			width: 1px;
			height: 1px;
			overflow: hidden;
			clip-path: inset(50%);
			white-space: nowrap;
		}
	}

	@media (min-width: 1024px) {
		.dn-player__tools {
			gap: 1rem;
		}
	}

	@media (min-width: 1280px) {
		.dn-player__grid {
			grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
		}
		.dn-player__tools {
			gap: 1.25rem;
		}
	}
</style>
