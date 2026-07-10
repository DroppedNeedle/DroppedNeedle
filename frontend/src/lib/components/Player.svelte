<script lang="ts">
	/*
	 * Player — orchestrates the rebuilt player system: the persistent PlayerDock
	 * and the full-screen PlayerStage. Owns the lyrics query, the YouTube
	 * pop-out mount, OS media-session integration (lock-screen / hardware keys),
	 * and the now-playing dominant tint that colors the dock rail + Stage wash.
	 */
	import { browser } from '$app/environment';
	import { onAudioEngineSuspended, resumeAudioEngine } from '$lib/player/audioElement';
	import { playerStore } from '$lib/stores/player.svelte';
	import { deckFocus } from '$lib/stores/deckFocus.svelte';
	import { playerUi } from '$lib/stores/playerUi.svelte';
	import { getLyricsQuery } from '$lib/queries/lyrics/LyricsQuery.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import PlayerDock from '$lib/components/player/PlayerDock.svelte';
	import PlayerStage from '$lib/components/player/PlayerStage.svelte';
	import YouTubePlayer from '$lib/components/YouTubePlayer.svelte';

	const lyricsQuery = getLyricsQuery(() => playerStore.nowPlaying);
	const supportsLyrics = $derived(lyricsQuery.isSuccess && lyricsQuery.data !== null);

	const playerVisible = $derived(
		playerStore.isPlayerVisible && playerStore.nowPlaying !== null && !deckFocus.inView
	);

	// the Stage cannot outlive the playback session
	$effect(() => {
		if (!playerVisible && playerUi.stageOpen) {
			playerUi.closeStage();
		}
	});

	// keep the YouTube pop-out above the Stage (YT policy forbids obscuring it)
	$effect(() => {
		document.documentElement.classList.toggle('dn-stage-open', playerUi.stageOpen);
		return () => document.documentElement.classList.remove('dn-stage-open');
	});

	/* ── background resilience: iOS suspends the AudioContext when the PWA hides,
	   which kills audio routed through the EQ chain. Recover the engine (and the
	   element, if suspension paused it) — but ONLY while the user still intends
	   playback. An explicit pause/stop must never be overridden. ─────────────── */
	function recoverPlayback(): void {
		if (!playerStore.intendedPlaying) return;
		void resumeAudioEngine();
		// Re-issue element play only in the foreground: while hidden, play() can
		// be rejected by autoplay policy and would feed the error chain.
		if (!document.hidden && !playerStore.isPlaying) playerStore.play();
	}

	$effect(() => {
		if (!browser) return;
		const onVisibilityChange = () => {
			if (!document.hidden) recoverPlayback();
		};
		document.addEventListener('visibilitychange', onVisibilityChange);
		const unsubscribeSuspend = onAudioEngineSuspended(recoverPlayback);
		return () => {
			document.removeEventListener('visibilitychange', onVisibilityChange);
			unsubscribeSuspend();
		};
	});

	/* ── OS media session: lock-screen artwork + hardware media keys ───────── */
	const supportsMediaSession = browser && 'mediaSession' in navigator;

	$effect(() => {
		if (!supportsMediaSession) return;
		const np = playerStore.nowPlaying;
		if (!np) {
			navigator.mediaSession.metadata = null;
			return;
		}
		const art = getCoverUrl(np.coverUrl, np.albumId);
		try {
			navigator.mediaSession.metadata = new MediaMetadata({
				title: np.trackName || np.albumName || 'DroppedNeedle',
				artist: np.artistName ?? '',
				album: np.albumName ?? '',
				artwork: art ? [{ src: new URL(art, window.location.href).href, sizes: '512x512' }] : []
			});
		} catch {
			/* MediaMetadata unavailable — nothing to surface */
		}
	});

	$effect(() => {
		if (!supportsMediaSession) return;
		navigator.mediaSession.playbackState = !playerStore.isPlayerVisible
			? 'none'
			: playerStore.isPlaying
				? 'playing'
				: 'paused';
	});

	$effect(() => {
		if (!supportsMediaSession) return;
		const duration = playerStore.duration;
		const position = playerStore.progress;
		if (duration > 0 && Number.isFinite(duration) && position >= 0 && position <= duration) {
			try {
				navigator.mediaSession.setPositionState({ duration, position, playbackRate: 1 });
			} catch {
				/* some engines reject transient states mid-load */
			}
		}
	});

	$effect(() => {
		if (!supportsMediaSession) return;
		const ms = navigator.mediaSession;
		ms.setActionHandler('play', () => playerStore.play());
		ms.setActionHandler('pause', () => playerStore.pause());
		ms.setActionHandler('stop', () => playerStore.stop());
		ms.setActionHandler('previoustrack', () => {
			if (playerStore.hasPrevious) playerStore.previousTrack();
		});
		ms.setActionHandler('nexttrack', () => {
			if (playerStore.hasNext) playerStore.nextTrack();
		});
		ms.setActionHandler('seekto', (details) => {
			if (playerStore.isSeekable && details.seekTime != null) {
				playerStore.seekTo(details.seekTime);
			}
		});
		return () => {
			const actions: MediaSessionAction[] = [
				'play',
				'pause',
				'stop',
				'previoustrack',
				'nexttrack',
				'seekto'
			];
			for (const action of actions) {
				try {
					ms.setActionHandler(action, null);
				} catch {
					/* action not supported */
				}
			}
		};
	});

	/* ── now-playing tint: average the artwork, feed --dn-now-tint ──────────── */
	let tintToken = 0;

	async function sampleTint(url: string): Promise<string | null> {
		return new Promise((resolve) => {
			const img = new Image();
			img.crossOrigin = 'anonymous';
			img.onload = () => {
				try {
					const size = 24;
					const canvas = document.createElement('canvas');
					canvas.width = size;
					canvas.height = size;
					const ctx = canvas.getContext('2d');
					if (!ctx) return resolve(null);
					ctx.drawImage(img, 0, 0, size, size);
					const data = ctx.getImageData(0, 0, size, size).data;
					let r = 0,
						g = 0,
						b = 0,
						count = 0;
					for (let i = 0; i < data.length; i += 4) {
						if (data[i + 3] < 128) continue;
						r += data[i];
						g += data[i + 1];
						b += data[i + 2];
						count++;
					}
					if (!count) return resolve(null);
					resolve(`${Math.round(r / count)} ${Math.round(g / count)} ${Math.round(b / count)}`);
				} catch {
					resolve(null); // tainted canvas (remote cover without CORS) — keep default tint
				}
			};
			img.onerror = () => resolve(null);
			img.src = url;
		});
	}

	$effect(() => {
		const np = playerStore.nowPlaying;
		const root = document.documentElement;
		if (!np) {
			root.style.removeProperty('--dn-now-tint');
			return;
		}
		const art = getCoverUrl(np.coverUrl, np.albumId);
		if (!art) {
			root.style.removeProperty('--dn-now-tint');
			return;
		}
		const token = ++tintToken;
		void sampleTint(art).then((tint) => {
			if (token !== tintToken) return; // a newer track superseded this sample
			if (tint) root.style.setProperty('--dn-now-tint', tint);
			else root.style.removeProperty('--dn-now-tint');
		});
	});
</script>

{#if playerVisible}
	<PlayerDock {supportsLyrics} />

	<PlayerStage
		{supportsLyrics}
		lyricsText={lyricsQuery.data?.text ?? ''}
		lyricLines={lyricsQuery.data?.lines ?? []}
		lyricsSynced={lyricsQuery.data?.is_synced ?? false}
		lyricsLoading={lyricsQuery.isFetching}
		lyricsError={lyricsQuery.isError}
	/>

	{#if playerStore.nowPlaying?.sourceType === 'youtube'}
		<YouTubePlayer />
	{/if}
{/if}
