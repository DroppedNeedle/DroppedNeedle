<script lang="ts">
	import { Music, Pause, User } from 'lucide-svelte';
	import AudioQualityBadge from '$lib/components/AudioQualityBadge.svelte';
	import type { NowPlayingSession } from '$lib/types';

	interface Props {
		sessions: NowPlayingSession[];
	}

	let { sessions }: Props = $props();

	function formatTime(ms: number): string {
		const totalSeconds = Math.floor(ms / 1000);
		const m = Math.floor(totalSeconds / 60);
		const s = totalSeconds % 60;
		return `${m}:${s.toString().padStart(2, '0')}`;
	}

	function progressPercent(session: NowPlayingSession): number | null {
		if (session.progress_ms == null || session.duration_ms == null || session.duration_ms <= 0)
			return null;
		return Math.min(100, (session.progress_ms / session.duration_ms) * 100);
	}

	const sourceLabels: Record<string, string> = {
		jellyfin: 'Jellyfin',
		navidrome: 'Navidrome',
		plex: 'Plex',
		local: 'DroppedNeedle',
		youtube: 'YouTube'
	};
</script>

{#if sessions.length > 0}
	<section class="space-y-3">
		<div class="flex items-center gap-2 px-1">
			<div class="now-playing-bars">
				<span></span><span></span><span></span>
			</div>
			<h2 class="text-lg font-semibold text-base-content sm:text-xl">Now Playing</h2>
		</div>

		<!-- auto-fit collapses empty tracks so one card fills the row, two share it, etc.;
		     the (100% - gaps)/4 floor caps it at four across before wrapping.
		     2.25rem = 3 * gap-3 (0.75rem); keep in sync if the gap changes. -->
		<div
			class="grid gap-3"
			style="grid-template-columns: repeat(auto-fit, minmax(max(17rem, calc((100% - 2.25rem) / 4)), 1fr));"
		>
			{#each sessions as session (session.id)}
				{@const progress = progressPercent(session)}
				<div
					class="flex h-full items-center gap-3 rounded-xl border border-base-content/5 bg-base-200/60 p-3 backdrop-blur-sm transition-colors hover:border-primary/20 {session.is_paused
						? 'opacity-70'
						: ''}"
				>
					<div class="relative h-[4.5rem] w-[4.5rem] shrink-0 overflow-hidden rounded-lg shadow">
						{#if session.cover_url}
							<img
								src={session.cover_url}
								alt={session.album_name}
								class="h-full w-full object-cover {session.is_paused ? 'grayscale' : ''}"
								loading="lazy"
							/>
						{:else}
							<div class="flex h-full w-full items-center justify-center bg-base-300">
								<Music class="h-6 w-6 text-base-content/40" />
							</div>
						{/if}
						{#if session.is_paused}
							<div class="absolute inset-0 flex items-center justify-center bg-base-300/50">
								<Pause class="h-5 w-5 text-base-content" />
							</div>
						{/if}
					</div>

					<div class="flex min-w-0 flex-1 flex-col gap-0.5">
						<div class="flex items-center gap-1.5">
							<div
								class="now-playing-bars now-playing-bars--sm {session.is_paused
									? 'now-playing-bars--paused'
									: ''}"
							>
								<span></span><span></span><span></span>
							</div>
							<span class="text-[10px] font-medium uppercase tracking-wide text-base-content/50">
								{session.is_paused ? 'Paused' : 'Now Playing'}
								{#if session.source}
									<span class="opacity-60">· {sourceLabels[session.source] ?? session.source}</span>
								{/if}
							</span>
						</div>

						{#if session.redacted}
							<p class="truncate text-sm font-medium italic leading-tight text-base-content/50">
								Private listening
							</p>
						{:else}
							<p class="truncate text-sm font-semibold leading-tight text-base-content">
								{session.track_name}
							</p>
							<p class="truncate text-xs text-base-content/60">{session.artist_name}</p>
						{/if}

						<div
							class="mt-0.5 flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-1 text-[10px] text-base-content/50"
						>
							{#if session.user_name}
								<span
									class="inline-flex items-center gap-1 rounded-full bg-primary/10 px-2 py-0.5 font-medium text-primary"
								>
									<User class="h-3 w-3" />
									{session.user_name}
								</span>
							{/if}
							{#if session.device_name}
								<span class="max-w-[8rem] truncate">{session.device_name}</span>
							{/if}
							{#if session.audio_codec}
								<AudioQualityBadge codec={session.audio_codec} bitrate={session.bitrate} compact />
							{/if}
						</div>

						{#if progress !== null}
							<div class="mt-1 flex items-center gap-1.5">
								<div class="h-1 flex-1 overflow-hidden rounded-full bg-base-content/10">
									<div
										class="h-full rounded-full bg-primary transition-[width] duration-1000 ease-linear"
										style="width: {progress}%"
									></div>
								</div>
								{#if session.progress_ms != null && session.duration_ms != null}
									<span class="shrink-0 text-[10px] tabular-nums text-base-content/40">
										{formatTime(session.progress_ms)}/{formatTime(session.duration_ms)}
									</span>
								{/if}
							</div>
						{/if}
					</div>
				</div>
			{/each}
		</div>
	</section>
{/if}
