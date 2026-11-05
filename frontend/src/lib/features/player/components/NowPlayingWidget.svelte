<!--
	What is playing in other sessions, one card per session, in the player
	bar's look: who, on what, which track and how far along. Paused cards
	are greyed with a pause mark. Read-only: no buttons, no links.
-->
<script lang="ts">
	import '$lib/features/player/components/now-playing.css';
	import { Pause, User } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import AudioQualityBadge from '$lib/features/player/components/AudioQualityBadge.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import type { NowPlayingSession } from '$lib/features/player/state/nowPlayingSessions.svelte';

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
		<SectionHead title="Now Playing">
			{#snippet chips()}
				<span class="now-playing-bars" aria-hidden="true"
					><span></span><span></span><span></span></span
				>
			{/snippet}
		</SectionHead>

		<!-- auto-fit collapses empty tracks so one card fills the row, two share it, etc.;
		     the (100% - gaps)/4 floor caps it at four across before wrapping.
		     2.25rem = 3 * gap-3 (0.75rem); keep in sync if the gap changes. -->
		<div
			class="grid grid-cols-[repeat(auto-fit,minmax(max(17rem,calc((100%_-_2.25rem)_/_4)),1fr))] gap-3"
		>
			{#each sessions as session (session.id)}
				{@const progress = progressPercent(session)}
				<div class="dn-np-card" class:dn-np-card--paused={session.is_paused}>
					<div class="relative shrink-0">
						<AlbumImage
							customUrl={session.cover_url ?? null}
							alt={session.album_name}
							size="full"
							rounded="sm"
							className="size-[4.5rem] {session.is_paused ? 'grayscale' : ''}"
						/>
						{#if session.is_paused}
							<div class="dn-np-card__paused">
								<Pause class="size-5 fill-current" aria-hidden="true" />
							</div>
						{/if}
					</div>

					<div class="flex min-w-0 flex-1 flex-col gap-0.5">
						<p class="dn-np-card__state">
							<span
								class="now-playing-bars now-playing-bars--sm"
								class:now-playing-bars--paused={session.is_paused}
								aria-hidden="true"><span></span><span></span><span></span></span
							>
							<span>{session.is_paused ? 'Paused' : 'Now Playing'}</span>
							{#if session.source}
								<span aria-hidden="true">·</span>
								<span class="truncate">{sourceLabels[session.source] ?? session.source}</span>
							{/if}
						</p>

						{#if session.redacted}
							<p class="truncate text-sm font-semibold text-muted italic">Private listening</p>
						{:else}
							<p class="truncate text-sm leading-tight font-bold">{session.track_name}</p>
							<p class="truncate text-xs text-muted">{session.artist_name}</p>
						{/if}

						<div class="mt-1 flex min-w-0 flex-wrap items-center gap-1.5 text-xs text-muted">
							{#if session.user_name}
								<StatusChip size="xs" icon={User}>{session.user_name}</StatusChip>
							{/if}
							{#if session.device_name}
								<span class="max-w-[8rem] truncate">{session.device_name}</span>
							{/if}
							{#if session.audio_codec}
								<AudioQualityBadge codec={session.audio_codec} bitrate={session.bitrate} compact />
							{/if}
						</div>

						{#if progress !== null}
							<div class="mt-1.5 flex items-center gap-2">
								<div class="dn-np-card__rail">
									<div class="dn-np-card__fill" style:width="{progress}%"></div>
								</div>
								{#if session.progress_ms != null && session.duration_ms != null}
									<span class="dn-np-card__time">
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

<style>
	.dn-np-card {
		display: flex;
		height: 100%;
		align-items: center;
		gap: 0.875rem;
		padding: 0.625rem 0.875rem 0.625rem 0.625rem;
		border: 1px solid var(--dn-line);
		border-radius: var(--radius-box);
		background: var(--color-base-200);
	}
	.dn-np-card--paused {
		border-style: dashed;
	}
	.dn-np-card__paused {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		border-radius: 0.25rem;
		background: color-mix(in oklab, var(--color-base-300) 55%, transparent);
		color: var(--color-base-content);
	}
	.dn-np-card__state {
		display: flex;
		min-width: 0;
		align-items: center;
		gap: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-np-card--paused :is(p, span) {
		color: var(--dn-muted);
	}
	.dn-np-card__rail {
		height: 4px;
		flex: 1;
		overflow: hidden;
		border-radius: 2px;
		background: var(--dn-line);
	}
	.dn-np-card__fill {
		height: 100%;
		border-radius: 2px;
		background: var(--dn-title);
		transition: width 1s linear;
	}
	.dn-np-card--paused .dn-np-card__fill {
		background: var(--dn-muted);
	}
	.dn-np-card__time {
		flex-shrink: 0;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-variant-numeric: tabular-nums;
		color: var(--dn-muted);
	}
</style>
