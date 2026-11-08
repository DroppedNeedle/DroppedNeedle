<!--
	The Listening Room's deck: one brushed-metal panel holding the room's
	title plate, the platter with its tonearm, and the controls (volume and
	tone knobs, the transport tray). The record spins while something plays
	and runs down when paused; the tonearm swings on with the music, lifts
	on a pause and goes back to its rest when nothing is loaded. Dropping a
	crate or search row anywhere on the deck plays it.
-->
<script lang="ts">
	import '$lib/features/player/components/now-playing.css';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { eqStore } from '$lib/features/player/state/eq.svelte';
	import { scrobbleManager } from '$lib/features/player/state/scrobble.svelte';
	import EqPanel from '$lib/features/player/components/EqPanel.svelte';
	import LyricsPanel from '$lib/features/player/components/LyricsPanel.svelte';
	import AudioQualityBadge from '$lib/features/player/components/AudioQualityBadge.svelte';
	import PlayerTrackMarks from '$lib/features/player/components/PlayerTrackMarks.svelte';
	import DeckVisualiser from '$lib/features/library/components/local/DeckVisualiser.svelte';
	import { coverTint } from '$lib/features/library/components/local/coverTint';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import BarcodeOrnament from '$lib/ui/BarcodeOrnament.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Knob from '$lib/ui/Knob.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { getLyricsQuery } from '$lib/features/player/queries/lyrics.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { navidromeFolderScope } from '$lib/features/remote-library/queries/RemoteQueries.svelte';
	import type { CrateTrack } from '$lib/features/library/queries/types';
	import type { SpinAlbum } from '$lib/features/library/queries/LocalV3Adapters';
	import { isValidMbid } from '$lib/utils/formatting';
	import { playedAlbumHref, playedArtistHref } from '$lib/utils/entityRoutes';
	import {
		Play,
		Pause,
		SkipBack,
		SkipForward,
		ChevronsLeft,
		ChevronsRight,
		Shuffle,
		Sparkles,
		Dices,
		ListMusic,
		SlidersHorizontal,
		Music2,
		Check,
		CircleX,
		Headphones
	} from 'lucide-svelte';

	interface Props {
		/** The library's counts for the title plate; null until they load. */
		stats: { tracks: number; artists: number; size: string } | null;
		reducedMotion?: boolean;
		onDropPlay: (track: CrateTrack) => void;
		onDropAlbum: (album: SpinAlbum) => void;
		onPlayAll: () => void;
		onShuffleAll: () => void;
		onSurprise: () => void;
		onOpenQueue: () => void;
	}

	let {
		stats,
		reducedMotion = false,
		onDropPlay,
		onDropAlbum,
		onPlayAll,
		onShuffleAll,
		onSurprise,
		onOpenQueue
	}: Props = $props();

	const uid = $props.id();
	const arcId = `dn-deck-arc-${uid}`;
	const np = $derived(playerStore.nowPlaying);
	const isLocal = $derived(np?.sourceType === 'local');
	const isPlaying = $derived(playerStore.isPlaying);
	const format = $derived(playerStore.currentQueueItem?.format ?? null);
	const isYouTube = $derived(np?.sourceType === 'youtube');
	const artistLink = $derived(playedArtistHref(np?.artistId));
	const albumLink = $derived(playedAlbumHref(np?.albumId, isLocal));

	let dragOver = $state(false);
	let eqPanelOpen = $state(false);
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

	// the glow behind the platter takes the playing cover's colour
	let tint = $state<string | null>(null);
	$effect(() => {
		const url = np?.coverUrl;
		if (!url) {
			tint = null;
			return;
		}
		let live = true;
		void coverTint(url).then((t) => {
			if (live) tint = t;
		});
		return () => {
			live = false;
		};
	});

	// The platter runs up to speed and coasts down rather than snapping.
	// slower than a real 33 so the sleeve on the label stays readable
	const DEG_PER_S = 100;
	let spinner = $state<HTMLDivElement>();
	let angle = 0;
	let speed = 0;
	$effect(() => {
		const el = spinner;
		const running = isPlaying && !reducedMotion;
		if (!el || reducedMotion) return;
		const target = running ? DEG_PER_S : 0;
		let raf = 0;
		let last = performance.now();
		const tick = (now: number) => {
			const dt = Math.min(0.1, (now - last) / 1000);
			last = now;
			const ease = running ? 2.6 : 1.4;
			speed += (target - speed) * Math.min(1, dt * ease);
			if (!running && speed < 0.5) speed = 0;
			angle = (angle + speed * dt) % 360;
			el.style.rotate = `${angle}deg`;
			if (running || speed > 0) raf = requestAnimationFrame(tick);
		};
		raf = requestAnimationFrame(tick);
		return () => cancelAnimationFrame(raf);
	});

	let scrubRatio = $state<number | null>(null);
	// The arm rides inward through the track: 13 degrees is the lead-in groove, 28 the run-out.
	const progressRatio = $derived(
		scrubRatio ??
			(playerStore.duration > 0 ? Math.min(1, playerStore.progress / playerStore.duration) : 0)
	);
	const shownProgress = $derived(
		scrubRatio === null ? playerStore.progress : scrubRatio * playerStore.duration
	);
	const armAngle = $derived(np ? 13 + progressRatio * 15 : 0);
	const armState = $derived(!np ? 'rest' : isPlaying ? 'play' : 'cue');

	function fmt(seconds: number): string {
		if (!Number.isFinite(seconds) || seconds < 0) return '0:00';
		const m = Math.floor(seconds / 60);
		const s = Math.floor(seconds % 60);
		return `${m}:${s.toString().padStart(2, '0')}`;
	}

	function seekRatio(e: Event): number {
		return Number((e.currentTarget as HTMLInputElement).value) / 1000;
	}

	// A drag moves only the thumb; the seek happens once, on release. The
	// drag value stays until `change` reads it (pointerup comes first).
	function onScrub(e: Event) {
		scrubRatio = seekRatio(e);
	}

	function onSeek(e: Event) {
		scrubRatio = null;
		const ratio = seekRatio(e);
		if (playerStore.duration > 0) playerStore.seekTo(ratio * playerStore.duration);
	}

	// Tone knobs drive one band each of the player's equaliser and switch it on.
	const BASS_BAND = 1;
	const TREBLE_BAND = 8;
	function setBand(band: number, dB: number) {
		if (!eqStore.enabled) eqStore.toggleEq();
		eqStore.setBandGain(band, dB);
	}
	const dB = (v: number) => `${v > 0 ? '+' : ''}${v} dB`;

	function readJson<T>(e: DragEvent, type: string): T | null {
		const raw = e.dataTransfer?.getData(type);
		if (!raw) return null;
		try {
			return JSON.parse(raw) as T;
		} catch {
			return null;
		}
	}

	function handleDragOver(e: DragEvent) {
		const types = e.dataTransfer?.types;
		if (
			types?.includes('application/x-crate-track') ||
			types?.includes('application/x-crate-album')
		) {
			e.preventDefault();
			dragOver = true;
		}
	}

	function handleDragLeave(e: DragEvent) {
		const next = e.relatedTarget as Node | null;
		if (next && (e.currentTarget as HTMLElement).contains(next)) return;
		dragOver = false;
	}

	function handleDrop(e: DragEvent) {
		e.preventDefault();
		dragOver = false;
		const track = readJson<CrateTrack>(e, 'application/x-crate-track');
		if (track) {
			onDropPlay(track);
			return;
		}
		const album = readJson<SpinAlbum>(e, 'application/x-crate-album');
		if (album) onDropAlbum(album);
	}
</script>

<section
	class="dn-deck"
	class:is-over={dragOver}
	style:--cover-tint={tint}
	aria-label="Now playing turntable - drop a track here to play it"
	ondragover={handleDragOver}
	ondragleave={handleDragLeave}
	ondrop={handleDrop}
>
	<span class="dn-deck__screw" data-at="tl" aria-hidden="true"></span>
	<span class="dn-deck__screw" data-at="tr" aria-hidden="true"></span>
	<span class="dn-deck__screw" data-at="bl" aria-hidden="true"></span>
	<span class="dn-deck__screw" data-at="br" aria-hidden="true"></span>

	<div class="dn-deck__grid">
		<!-- the title plate -->
		<div class="dn-deck__plate">
			<Headphones class="dn-deck__plate-icon" aria-hidden="true" />
			<h1 class="dn-deck__title">The Listening Room</h1>
			<p class="dn-deck__stats">
				{#if stats}
					<span>{stats.tracks.toLocaleString()} tracks</span><span class="dn-deck__dot"
						>&middot;</span
					><span>{stats.artists.toLocaleString()} artists</span><span class="dn-deck__dot"
						>&middot;</span
					><span>{stats.size}</span>
				{:else}
					Your local music, ready to spin
				{/if}
			</p>
			<div class="dn-deck__badge" aria-hidden="true">
				<BarcodeOrnament class="h-7 text-base-content" />
				<span>DN / CC 3.2</span>
			</div>
			<p class="dn-deck__model" aria-hidden="true">
				DroppedNeedle<br />Listening component<br />Model LR-3.2
			</p>
		</div>

		<!-- the platter and its tonearm -->
		<div class="dn-deck__stage">
			<div class="dn-deck__glow" class:is-on={isPlaying} aria-hidden="true"></div>
			<div class="dn-platter" data-theme="dark">
				<div bind:this={spinner} class="dn-platter__spin">
					<div class="dn-platter__strobe" aria-hidden="true"></div>
					<div class="dn-platter__record">
						<div class="dn-platter__label">
							{#if np}
								<AlbumImage
									mbid={isValidMbid(np.albumId) ? np.albumId : ''}
									remoteUrl={np.coverRemoteUrl?.startsWith('http') ? np.coverRemoteUrl : null}
									customUrl={np.coverUrl}
									alt={np.albumName ?? 'Album'}
									size="full"
									requestSize={250}
									responsiveSizes="(max-width: 639px) 40vw, 220px"
									lazy={false}
									rounded="none"
									className="h-full w-full object-cover"
								/>
							{:else}
								<svg class="dn-platter__blank" viewBox="0 0 100 100" aria-hidden="true">
									<defs>
										<path id={arcId} d="M50 50 m-35 0 a35 35 0 1 1 70 0 a35 35 0 1 1 -70 0" />
									</defs>
									<text>
										<textPath href="#{arcId}">DROPPEDNEEDLE · THE LISTENING ROOM ·</textPath>
									</text>
									<circle cx="50" cy="50" r="22" class="dn-platter__blank-ring" />
								</svg>
							{/if}
						</div>
					</div>
				</div>
				<div class="dn-platter__sheen" aria-hidden="true"></div>
				<div class="dn-platter__spindle" aria-hidden="true"></div>
			</div>

			<svg class="dn-arm" data-state={armState} viewBox="0 0 100 100" aria-hidden="true">
				<circle class="dn-arm__rest" cx="92" cy="50" r="2" />
				<rect class="dn-arm__rest" x="91" y="50" width="2" height="6" rx="0.6" />
				<circle class="dn-arm__base" cx="92" cy="8" r="5.6" />
				<circle class="dn-arm__base-ring" cx="92" cy="8" r="3.8" />
				<g class="dn-arm__swing" style:--arm-angle="{armAngle}deg">
					<rect class="dn-arm__weight" x="89" y="-1.5" width="6" height="6.5" rx="1.4" />
					<path class="dn-arm__tube" d="M92 8 L92 58 L89 65" />
					<g transform="translate(88.6 66) rotate(24)">
						<rect class="dn-arm__shell" x="-2.4" y="-2.6" width="4.8" height="7.4" rx="0.9" />
						<path class="dn-arm__lift" d="M2.4 -1.6 l2.8 -1.4" />
					</g>
					<circle class="dn-arm__pivot" cx="92" cy="8" r="2.4" />
				</g>
			</svg>

			{#if dragOver}
				<div class="dn-deck__drop" aria-hidden="true">
					<span>Drop to play</span>
				</div>
			{/if}
		</div>

		<!-- the controls -->
		<div class="dn-deck__controls">
			<div class="dn-deck__knobs">
				<Knob
					label="Volume"
					value={playerStore.volume}
					onchange={(v) => playerStore.setVolume(v)}
					keyStep={5}
					marks={[
						{ at: 0, text: '0' },
						{ at: 50, text: '5' },
						{ at: 100, text: '10' }
					]}
					format={(v) => `${Math.round(v)}%`}
				/>
				<div class="dn-deck__tone">
					<Knob
						size="sm"
						label="Bass"
						value={eqStore.gains[BASS_BAND] ?? 0}
						onchange={(v) => setBand(BASS_BAND, v)}
						min={-12}
						max={12}
						origin={0}
						resetTo={0}
						dots={9}
						format={dB}
						disabled={isYouTube}
						disabledReason="EQ unavailable for YouTube"
					/>
					<Knob
						size="sm"
						label="Treble"
						value={eqStore.gains[TREBLE_BAND] ?? 0}
						onchange={(v) => setBand(TREBLE_BAND, v)}
						min={-12}
						max={12}
						origin={0}
						resetTo={0}
						dots={9}
						format={dB}
						disabled={isYouTube}
						disabledReason="EQ unavailable for YouTube"
					/>
					<div
						class="tooltip tooltip-top dn-deck__eq"
						data-tip={isYouTube ? 'EQ unavailable for YouTube' : 'Equalizer'}
					>
						<IconButton
							size="sm"
							icon={SlidersHorizontal}
							label="Toggle equalizer"
							active={eqStore.enabled && !isYouTube}
							disabled={isYouTube}
							aria-expanded={eqPanelOpen}
							onclick={() => (eqPanelOpen = !eqPanelOpen)}
						/>
					</div>
				</div>
			</div>

			<div class="dn-deck__tray">
				{#if np}
					<div class="dn-deck__display" data-theme="dark">
						<DeckVisualiser {reducedMotion} />
						<div class="dn-deck__now">
							<div class="dn-deck__thumb">
								<AlbumImage
									mbid={isValidMbid(np.albumId) ? np.albumId : ''}
									remoteUrl={np.coverRemoteUrl?.startsWith('http') ? np.coverRemoteUrl : null}
									customUrl={np.coverUrl}
									alt=""
									size="full"
									requestSize={250}
									lazy={false}
									rounded="none"
									className="h-full w-full object-cover"
								/>
							</div>
							<div class="min-w-0 flex-1">
								<p class="dn-deck__track">
									{#if isPlaying}
										<span class="now-playing-bars now-playing-bars--sm" aria-hidden="true">
											<span></span><span></span><span></span>
										</span>
									{/if}
									<span class="truncate">{np.trackName ?? np.albumName}</span>
								</p>
								<p class="dn-deck__artist">
									{#if artistLink}<a href={artistLink} class="dn-deck__link">{np.artistName}</a
										>{:else}{np.artistName}{/if}{#if np.albumName}<span
											>&nbsp;&middot;
											{#if albumLink}<a href={albumLink} class="dn-deck__link">{np.albumName}</a
												>{:else}{np.albumName}{/if}</span
										>{/if}
								</p>
							</div>
						</div>
						<PlayerTrackMarks alwaysStars />
					</div>
					<div class="dn-deck__seek">
						<span>{fmt(shownProgress)}</span>
						<input
							type="range"
							min="0"
							max="1000"
							value={Math.round(progressRatio * 1000)}
							oninput={onScrub}
							onchange={onSeek}
							onpointercancel={() => (scrubRatio = null)}
							onblur={() => (scrubRatio = null)}
							disabled={!playerStore.isSeekable}
							aria-label="Seek"
							class="range range-xs range-primary flex-1"
						/>
						<span>{fmt(playerStore.duration)}</span>
					</div>

					<div class="dn-deck__transport">
						<IconButton
							size="sm"
							icon={ChevronsLeft}
							label="Previous album"
							disabled={!playerStore.hasPreviousAlbum}
							onclick={() => playerStore.previousAlbum()}
						/>
						<IconButton
							icon={SkipBack}
							label="Previous"
							disabled={!playerStore.hasPrevious}
							onclick={() => playerStore.previousTrack()}
						/>
						<IconButton
							variant="primary"
							size="lg"
							icon={isPlaying ? Pause : Play}
							label={isPlaying ? 'Pause' : 'Play'}
							onclick={() => playerStore.togglePlay()}
						/>
						<IconButton
							icon={SkipForward}
							label="Next"
							disabled={!playerStore.hasNext}
							onclick={() => playerStore.nextTrack()}
						/>
						<IconButton
							size="sm"
							icon={ChevronsRight}
							label="Next album"
							disabled={!playerStore.hasNextAlbum}
							onclick={() => playerStore.nextAlbum()}
						/>
					</div>

					<div class="dn-deck__utility">
						<IconButton
							size="sm"
							icon={Shuffle}
							label="Shuffle"
							active={playerStore.shuffleEnabled}
							aria-pressed={playerStore.shuffleEnabled}
							onclick={() => playerStore.toggleShuffle()}
						/>
						<div class="indicator">
							{#if playerStore.upcomingQueueLength > 0}
								<span class="badge indicator-item badge-xs badge-primary"
									>{playerStore.upcomingQueueLength}</span
								>
							{/if}
							<IconButton size="sm" icon={ListMusic} label="Open queue" onclick={onOpenQueue} />
						</div>
						{#if supportsLyrics}
							<div class="tooltip tooltip-top" data-tip="Lyrics">
								<IconButton
									size="sm"
									icon={Music2}
									label="Toggle lyrics"
									active={lyricsPanelOpen}
									aria-expanded={lyricsPanelOpen}
									onclick={() => (lyricsPanelOpen = !lyricsPanelOpen)}
								/>
							</div>
						{/if}
						<div class="ml-auto flex max-w-full flex-wrap items-center gap-1.5">
							{#if format}
								<AudioQualityBadge codec={format} compact />
							{/if}
							{#if scrobbleManager.enabled && scrobbleManager.status !== 'idle'}
								<div class="tooltip tooltip-top" data-tip={scrobbleManager.tooltip}>
									{#if scrobbleManager.status === 'scrobbled'}
										<Check class="size-4 text-success" aria-label="Scrobbled" />
									{:else if scrobbleManager.status === 'error'}
										<CircleX class="size-4 text-error" aria-label="Scrobble failed" />
									{:else}
										<StatusChip tone="info" size="xs">Tracking</StatusChip>
									{/if}
								</div>
							{/if}
						</div>
					</div>

					{#if isLocal}
						<p class="dn-deck__hint">drag a record onto the deck</p>
					{/if}
				{:else}
					<h2 class="dn-deck__cue">Drop the needle</h2>
					<p class="dn-deck__lede">Drag a record from the crate onto the deck, or just hit play.</p>
					<div class="dn-deck__buttons">
						<Button variant="primary" size="display-sm" icon={Play} onclick={onPlayAll}
							>Play All</Button
						>
						<Button variant="outline" size="sm" icon={Shuffle} onclick={onShuffleAll}
							>Shuffle</Button
						>
						<Button variant="ghost" size="sm" icon={Dices} onclick={onSurprise}>Surprise me</Button>
					</div>
					<p class="dn-deck__hint">
						<Sparkles class="size-3" aria-hidden="true" /> your library, ready to spin
					</p>
				{/if}
			</div>
		</div>
	</div>
</section>

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

<style>
	/* ---- the panel: brushed metal in the theme's own colours ---- */
	.dn-deck {
		--metal: var(--color-base-200);
		--metal-hi: var(--color-base-300);
		--bevel-hi: rgb(255 255 255 / 0.07);
		--bevel-lo: rgb(0 0 0 / 0.55);
		--brush: rgb(255 255 255 / 0.018);
		position: relative;
		container: deck / inline-size;
		outline: 2px solid transparent;
		outline-offset: 4px;
		transition: outline-color 0.2s ease;
		border-radius: 1.25rem;
		border: 1px solid var(--dn-line);
		padding: 1.25rem;
		background:
			repeating-linear-gradient(
				0deg,
				var(--brush) 0 1px,
				transparent 1px 3px,
				rgb(0 0 0 / 0.02) 3px 4px
			),
			linear-gradient(
				172deg,
				color-mix(in oklab, var(--metal-hi) 100%, rgb(255 255 255 / 0.04)) 0%,
				var(--metal) 46%,
				color-mix(in oklab, var(--metal) 80%, var(--color-base-100)) 100%
			);
		box-shadow:
			inset 0 1px 0 var(--bevel-hi),
			inset 0 -2px 0 var(--bevel-lo),
			inset 0 0 0 4px color-mix(in oklab, var(--metal) 60%, transparent),
			0 28px 50px -32px rgb(0 0 0 / 0.9);
	}
	:global([data-theme='light']) .dn-deck {
		--metal: var(--color-base-300);
		--metal-hi: var(--color-base-200);
		--bevel-hi: rgb(255 255 255 / 0.9);
		--bevel-lo: rgb(0 0 0 / 0.08);
		--brush: rgb(0 0 0 / 0.025);
		box-shadow:
			inset 0 1px 0 var(--bevel-hi),
			inset 0 -2px 0 var(--bevel-lo),
			0 24px 40px -30px rgb(0 0 0 / 0.35);
	}
	@media (min-width: 640px) {
		.dn-deck {
			padding: 1.75rem;
		}
	}
	.dn-deck.is-over {
		outline-color: var(--dn-action);
	}

	.dn-deck__screw {
		position: absolute;
		width: 0.75rem;
		height: 0.75rem;
		border-radius: 9999px;
		background:
			linear-gradient(
				var(--slot, 45deg),
				transparent 42%,
				rgb(0 0 0 / 0.55) 42% 58%,
				transparent 58%
			),
			radial-gradient(circle at 35% 30%, rgb(255 255 255 / 0.35), transparent 70%),
			color-mix(in oklab, var(--color-base-content) 30%, var(--metal));
		box-shadow:
			inset 0 0 0 1px rgb(0 0 0 / 0.35),
			0 1px 0 var(--bevel-hi);
		pointer-events: none;
	}
	.dn-deck__screw[data-at='tl'] {
		top: 0.625rem;
		left: 0.625rem;
		--slot: 30deg;
	}
	.dn-deck__screw[data-at='tr'] {
		top: 0.625rem;
		right: 0.625rem;
		--slot: 115deg;
	}
	.dn-deck__screw[data-at='bl'] {
		bottom: 0.625rem;
		left: 0.625rem;
		--slot: 160deg;
	}
	.dn-deck__screw[data-at='br'] {
		bottom: 0.625rem;
		right: 0.625rem;
		--slot: 70deg;
	}

	/* ---- layout: stacked, then plate | platter | controls when there is room ---- */
	.dn-deck__grid {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		grid-template-areas: 'plate' 'stage' 'controls';
		gap: 1.25rem;
	}
	.dn-deck__plate {
		grid-area: plate;
	}
	.dn-deck__stage {
		grid-area: stage;
	}
	.dn-deck__controls {
		grid-area: controls;
	}

	@container deck (min-width: 54rem) {
		.dn-deck__grid {
			grid-template-columns: 8.75rem minmax(0, 1fr) 15rem;
			grid-template-areas: 'plate stage controls';
			align-items: stretch;
			gap: 1.5rem;
		}
	}

	/* ---- title plate ---- */
	.dn-deck__plate {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		column-gap: 0.75rem;
		align-items: center;
	}
	.dn-deck__plate :global(.dn-deck__plate-icon) {
		grid-row: span 2;
		width: 2rem;
		height: 2rem;
		color: var(--dn-title);
	}
	.dn-deck__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 2rem;
		line-height: 0.95;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		font-synthesis: none;
		color: var(--dn-title);
	}
	.dn-deck__stats {
		margin-top: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--color-muted);
		font-variant-numeric: tabular-nums;
	}
	.dn-deck__dot {
		margin-inline: 0.4em;
	}
	.dn-deck__badge,
	.dn-deck__model {
		display: none;
	}

	@container deck (min-width: 54rem) {
		.dn-deck__plate {
			display: flex;
			flex-direction: column;
			align-items: flex-start;
			padding-top: 0.25rem;
		}
		.dn-deck__plate :global(.dn-deck__plate-icon) {
			width: 2.25rem;
			height: 2.25rem;
			margin-bottom: 0.875rem;
		}
		.dn-deck__title {
			font-size: 2.35rem;
			line-height: 0.92;
		}
		.dn-deck__stats {
			margin-top: 1rem;
			display: flex;
			flex-direction: column;
			gap: 0.2rem;
			line-height: 1.35;
		}
		.dn-deck__dot {
			display: none;
		}
		.dn-deck__badge {
			display: flex;
			flex-direction: column;
			gap: 0.375rem;
			width: 100%;
			margin-top: 1.25rem;
			padding-top: 1.25rem;
			border-top: 1px solid var(--dn-line);
			font-family: var(--font-mono);
			font-size: 0.6875rem;
			font-weight: 600;
			letter-spacing: 0.14em;
			color: var(--color-base-content);
		}
		.dn-deck__model {
			display: block;
			margin-top: auto;
			padding-top: 1.5rem;
			font-family: var(--font-mono);
			font-size: 0.625rem;
			line-height: 1.6;
			letter-spacing: 0.16em;
			text-transform: uppercase;
			color: var(--color-muted);
		}
	}

	/* ---- the platter: a black record on a strobed metal platter ---- */
	.dn-deck__stage {
		position: relative;
		width: min(100%, 34rem);
		margin-inline: auto;
		aspect-ratio: 1;
	}
	@container deck (min-width: 54rem) {
		.dn-deck__stage {
			width: calc(100% + 3rem);
			max-width: 46rem;
			margin-inline: -1.5rem;
			align-self: center;
		}
	}

	/* record: 88% of the stage, left of centre, so the arm has its corner */
	.dn-platter,
	.dn-deck__glow,
	.dn-deck__drop {
		position: absolute;
		left: 0;
		top: 8%;
		width: 90%;
		aspect-ratio: 1;
		border-radius: 9999px;
	}

	.dn-deck__glow {
		inset-inline-start: -6%;
		top: 2%;
		width: 100%;
		background: radial-gradient(
			circle,
			color-mix(in oklab, var(--cover-tint, var(--color-primary)) 26%, transparent) 30%,
			transparent 70%
		);
		opacity: 0.35;
		transition: opacity 1.2s ease;
		pointer-events: none;
	}
	.dn-deck__glow.is-on {
		opacity: 1;
	}

	.dn-platter {
		background:
			radial-gradient(circle, transparent 0 94.5%, rgb(0 0 0 / 0.5) 95%, transparent 96%),
			conic-gradient(
				from 30deg,
				transparent 0deg,
				rgb(255 255 255 / 0.14) 40deg,
				transparent 90deg,
				rgb(255 255 255 / 0.06) 200deg,
				transparent 260deg
			),
			radial-gradient(circle, var(--color-base-300) 0 80%, var(--color-neutral) 100%);
		box-shadow:
			0 1.5rem 2.5rem -1rem rgb(0 0 0 / 0.85),
			0 0 0 1px rgb(0 0 0 / 0.6),
			inset 0 0 0 1px rgb(255 255 255 / 0.1);
	}

	.dn-platter__spin {
		position: absolute;
		inset: 0;
		border-radius: inherit;
		will-change: rotate;
	}

	/* the strobe marks round the platter's edge */
	.dn-platter__strobe {
		position: absolute;
		inset: 0.6%;
		border-radius: inherit;
		background: repeating-conic-gradient(
			rgb(255 255 255 / 0.2) 0 0.45deg,
			transparent 0.45deg 2.4deg
		);
		mask: radial-gradient(circle closest-side, transparent 95%, black 95.5% 98.5%, transparent 99%);
		-webkit-mask: radial-gradient(
			circle closest-side,
			transparent 95%,
			black 95.5% 98.5%,
			transparent 99%
		);
	}

	.dn-platter__record {
		container-type: inline-size;
		position: absolute;
		inset: 7%;
		border-radius: inherit;
		background:
			repeating-radial-gradient(
				circle closest-side,
				rgb(255 255 255 / 0.045) 0 0.18cqw,
				transparent 0.18cqw 0.55cqw
			),
			repeating-radial-gradient(
				circle closest-side,
				transparent 0 4.6cqw,
				rgb(0 0 0 / 0.5) 4.6cqw 5cqw,
				transparent 5cqw 8.2cqw
			),
			radial-gradient(
				circle closest-side,
				var(--dn-vinyl) 0 34%,
				color-mix(in oklab, var(--dn-vinyl-groove) 50%, var(--dn-vinyl)) 70%,
				var(--dn-vinyl) 96%,
				var(--dn-vinyl-groove) 99%,
				var(--dn-vinyl) 100%
			);
		box-shadow:
			0 0 0 1px rgb(0 0 0 / 0.7),
			0 0.4rem 1rem rgb(0 0 0 / 0.6);
	}

	.dn-platter__label {
		position: absolute;
		inset: 33%;
		overflow: hidden;
		border-radius: 9999px;
		box-shadow:
			0 0 0 0.25cqw rgb(0 0 0 / 0.8),
			0 0 0 0.9cqw color-mix(in oklab, var(--dn-vinyl-groove) 70%, var(--dn-vinyl));
	}
	/* nothing loaded: the house label, acid with a printed ring of type */
	.dn-platter__blank {
		width: 100%;
		height: 100%;
		background:
			radial-gradient(circle at 35% 30%, rgb(255 255 255 / 0.2), transparent 60%),
			color-mix(in oklab, var(--dn-action) 78%, var(--dn-vinyl-groove));
		rotate: -24deg;
	}
	.dn-platter__blank text {
		fill: color-mix(in oklab, var(--dn-action-content) 75%, transparent);
		font-family: var(--font-mono);
		font-size: 6.6px;
		font-weight: 700;
		letter-spacing: 0.12em;
	}
	.dn-platter__blank-ring {
		fill: none;
		stroke: color-mix(in oklab, var(--dn-action-content) 35%, transparent);
		stroke-width: 0.6;
	}

	/* light that stays put while the record turns under it */
	.dn-platter__sheen {
		position: absolute;
		inset: 7%;
		border-radius: inherit;
		pointer-events: none;
		background: conic-gradient(
			from 20deg,
			transparent 0deg,
			rgb(255 255 255 / 0.1) 24deg,
			transparent 52deg,
			transparent 196deg,
			rgb(255 255 255 / 0.07) 222deg,
			transparent 252deg
		);
		mask: radial-gradient(circle closest-side, transparent 34%, black 35%);
		-webkit-mask: radial-gradient(circle closest-side, transparent 34%, black 35%);
	}

	.dn-platter__spindle {
		position: absolute;
		left: 50%;
		top: 50%;
		width: 2.2%;
		aspect-ratio: 1;
		translate: -50% -50%;
		border-radius: 9999px;
		background: radial-gradient(
			circle at 35% 30%,
			rgb(255 255 255 / 0.9),
			color-mix(in oklab, var(--color-base-content) 50%, black) 70%
		);
		box-shadow: 0 1px 2px rgb(0 0 0 / 0.8);
	}

	/* ---- tonearm ---- */
	.dn-arm {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		overflow: visible;
		pointer-events: none;
		filter: drop-shadow(0 0.35rem 0.35rem rgb(0 0 0 / 0.55));
		transition: filter 0.5s ease;
	}
	.dn-arm[data-state='cue'] {
		filter: drop-shadow(0.4rem 0.9rem 0.5rem rgb(0 0 0 / 0.45));
	}
	.dn-arm__swing {
		transform-origin: 92px 8px;
		transform-box: view-box;
		transform: rotate(0deg);
		transition: transform 1.4s var(--ease-spring);
	}
	.dn-arm[data-state='play'] .dn-arm__swing,
	.dn-arm[data-state='cue'] .dn-arm__swing {
		transform: rotate(var(--arm-angle));
	}
	.dn-arm__base {
		fill: color-mix(in oklab, var(--color-base-content) 22%, var(--metal));
		stroke: rgb(0 0 0 / 0.45);
		stroke-width: 0.4;
	}
	.dn-arm__base-ring {
		fill: var(--dn-vinyl);
		stroke: rgb(255 255 255 / 0.18);
		stroke-width: 0.35;
	}
	.dn-arm__rest {
		fill: color-mix(in oklab, var(--color-base-content) 35%, var(--metal));
	}
	.dn-arm__weight {
		fill: var(--dn-vinyl-groove);
		stroke: rgb(255 255 255 / 0.25);
		stroke-width: 0.35;
	}
	.dn-arm__tube {
		fill: none;
		stroke: color-mix(in oklab, white 62%, var(--color-base-300));
		stroke-width: 1.25;
		stroke-linecap: round;
		stroke-linejoin: round;
	}
	:global([data-theme='light']) .dn-arm__tube {
		stroke: color-mix(in oklab, var(--color-base-content) 70%, white);
	}
	.dn-arm__shell {
		fill: var(--dn-vinyl);
		stroke: rgb(255 255 255 / 0.3);
		stroke-width: 0.35;
	}
	.dn-arm__lift {
		fill: none;
		stroke: color-mix(in oklab, white 70%, var(--color-base-content));
		stroke-width: 0.8;
		stroke-linecap: round;
	}
	.dn-arm__pivot {
		fill: var(--dn-action);
	}

	/* ---- drop target ---- */
	.dn-deck__drop {
		display: grid;
		place-items: center;
		pointer-events: none;
		border: 3px dashed var(--dn-action);
		background: color-mix(in oklab, var(--dn-action) 12%, transparent);
	}
	.dn-deck__drop span {
		padding: 0.5rem 1rem;
		border-radius: 9999px;
		background: var(--dn-action);
		color: var(--dn-action-content);
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}

	/* ---- controls ---- */
	.dn-deck__controls {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		min-width: 0;
	}
	.dn-deck__knobs {
		display: flex;
		align-items: flex-start;
		justify-content: center;
		gap: 0.75rem;
	}
	.dn-deck__tone {
		display: grid;
		grid-template-columns: auto auto;
		gap: 0.5rem 0.75rem;
		align-items: start;
		justify-items: center;
	}
	.dn-deck__eq {
		grid-column: span 2;
	}

	@container deck (min-width: 34rem) and (max-width: 53.99rem) {
		.dn-deck__controls {
			display: grid;
			grid-template-columns: auto minmax(0, 1fr);
			gap: 1.5rem;
			align-items: start;
		}
	}
	@container deck (max-width: 33.99rem) {
		.dn-deck__tone {
			grid-template-columns: auto auto auto;
			align-items: center;
		}
		.dn-deck__eq {
			grid-column: auto;
			grid-row: 1;
			grid-column-start: 3;
			align-self: center;
		}
		.dn-deck__knobs :global(.dn-knob[data-size='lg']) {
			--knob-size: 7rem;
		}
	}
	@container deck (max-width: 21.99rem) {
		.dn-deck__knobs {
			gap: 0.5rem;
		}
		.dn-deck__tone {
			grid-template-columns: auto auto;
			gap: 0.25rem 0.5rem;
		}
		.dn-deck__eq {
			grid-row: auto;
			grid-column: span 2;
		}
		.dn-deck__knobs :global(.dn-knob[data-size='lg']) {
			--knob-size: 5.75rem;
		}
		.dn-deck__knobs :global(.dn-knob[data-size='sm']) {
			--knob-size: 3.75rem;
		}
	}
	@container deck (min-width: 54rem) {
		.dn-deck__knobs {
			justify-content: space-between;
			gap: 0.5rem;
		}
		.dn-deck__tone {
			grid-template-columns: auto;
			gap: 0.25rem;
		}
		.dn-deck__tone :global(.dn-knob[data-size='sm']) {
			--knob-size: 4.25rem;
		}
		.dn-deck__eq {
			grid-column: auto;
		}
	}

	/* the inset tray for what's on the deck */
	.dn-deck__tray {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding: 1rem;
		border-radius: 0.875rem;
		background: color-mix(in oklab, var(--color-base-100) 70%, transparent);
		box-shadow:
			inset 0 2px 6px rgb(0 0 0 / 0.45),
			inset 0 0 0 1px rgb(0 0 0 / 0.35),
			0 1px 0 var(--bevel-hi);
	}
	:global([data-theme='light']) .dn-deck__tray {
		background: color-mix(in oklab, var(--color-base-100) 80%, var(--color-base-300));
		box-shadow:
			inset 0 2px 5px rgb(0 0 0 / 0.1),
			inset 0 0 0 1px rgb(0 0 0 / 0.08),
			0 1px 0 var(--bevel-hi);
	}
	@container deck (min-width: 54rem) {
		.dn-deck__controls {
			justify-content: space-between;
		}
		.dn-deck__tray {
			padding: 0.875rem;
		}
	}

	.dn-deck__cue {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 1.75rem;
		line-height: 1;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		font-synthesis: none;
		color: var(--color-base-content);
	}
	.dn-deck__lede {
		margin-top: -0.25rem;
		font-size: 0.875rem;
		line-height: 1.4;
		color: var(--color-muted);
	}
	.dn-deck__buttons {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 0.375rem;
	}
	@container deck (min-width: 34rem) and (max-width: 53.99rem) {
		.dn-deck__buttons {
			grid-template-columns: 1fr 1fr;
		}
		.dn-deck__buttons > :global(:first-child) {
			grid-column: span 2;
		}
	}
	.dn-deck__hint {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.375rem;
		margin-top: auto;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		text-align: center;
		color: var(--color-muted);
	}

	/* the display window: dark glass in both themes, the spectrum behind */
	.dn-deck__display {
		position: relative;
		isolation: isolate;
		overflow: hidden;
		display: flex;
		flex-direction: column;
		gap: 0.625rem;
		padding: 0.75rem;
		border-radius: 0.625rem;
		background:
			linear-gradient(180deg, rgb(255 255 255 / 0.04), transparent 40%), var(--color-base-100);
		color: var(--color-base-content);
		box-shadow: inset 0 0 0 1px rgb(255 255 255 / 0.06);
	}
	.dn-deck__now {
		display: flex;
		align-items: center;
		gap: 0.625rem;
		min-width: 0;
	}
	.dn-deck__thumb {
		width: 2.75rem;
		height: 2.75rem;
		flex: none;
		overflow: hidden;
		border-radius: 0.25rem;
		box-shadow: 0 0 0 1px rgb(255 255 255 / 0.12);
	}
	.dn-deck__track {
		display: flex;
		align-items: center;
		gap: 0.375rem;
		min-width: 0;
		font-size: 0.9375rem;
		font-weight: 700;
		line-height: 1.25;
	}
	.dn-deck__artist {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 0.8125rem;
		color: var(--dn-muted);
	}
	.dn-deck__link {
		color: inherit;
	}
	.dn-deck__link:hover,
	.dn-deck__link:focus-visible {
		text-decoration: underline;
	}
	.dn-deck__seek {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-variant-numeric: tabular-nums;
		color: var(--dn-muted);
	}
	.dn-deck__seek span {
		min-width: 2.25rem;
	}
	.dn-deck__seek span:last-child {
		text-align: right;
	}

	.dn-deck__transport {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.25rem;
	}
	.dn-deck__utility {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.25rem;
		min-width: 0;
	}

	@media (forced-colors: active) {
		.dn-deck:not(.is-over) {
			outline: none;
		}
		.dn-platter,
		.dn-platter__record,
		.dn-platter__label {
			border: 2px solid CanvasText;
		}
		.dn-deck__screw,
		.dn-deck__glow,
		.dn-platter__sheen {
			display: none;
		}
	}
	@media (prefers-contrast: more) {
		.dn-deck__screw,
		.dn-deck__glow,
		.dn-platter__sheen,
		.dn-platter__strobe {
			display: none;
		}
		.dn-deck {
			background: var(--color-base-200);
			border-color: var(--color-base-content);
		}
	}
</style>
