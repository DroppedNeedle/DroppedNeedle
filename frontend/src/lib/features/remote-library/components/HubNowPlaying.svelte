<!--
	Who is playing on this server now, one row per session: the cover, Now
	Playing or Paused, the track, who and on what, and how far along.
	Read-only. Shown only while a session is open.
-->
<script lang="ts">
	import { ChartNoAxesColumn, Laptop, Pause, Play, User } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import AudioQualityBadge from '$lib/features/player/components/AudioQualityBadge.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import type { nowPlayingMerged } from '$lib/features/player/state/nowPlayingMerged.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';

	type NowPlayingSession = ReturnType<typeof nowPlayingMerged.sessionsForSource>[number];

	interface Props {
		sessions: NowPlayingSession[];
		sourceLabel: string;
	}

	let { sessions, sourceLabel }: Props = $props();

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
</script>

{#if sessions.length > 0}
	<section class="hub-np">
		<SectionHead title="Now Playing" icon={ChartNoAxesColumn} />

		<div class="hub-panel hub-np__list">
			{#each sessions as session (session.id)}
				{@const progress = progressPercent(session)}
				<div class="hub-np__row" class:hub-np__row--paused={session.is_paused}>
					<div class="hub-np__cover">
						<AlbumImage
							customUrl={session.cover_url || null}
							alt={session.album_name}
							size="full"
							rounded="none"
							className="h-full w-full {session.is_paused ? 'grayscale' : ''}"
						/>
					</div>

					<div class="hub-np__main">
						<div class="hub-np__state">
							{#if session.is_paused}
								<StatusChip size="xs" icon={Pause}>Paused</StatusChip>
							{:else}
								<StatusChip size="xs" tone="action" icon={Play}>Now Playing</StatusChip>
							{/if}
							<span class="hub-np__source">{sourceLabel}</span>
							{#if session.user_name}
								<StatusChip size="xs" icon={User} class="ml-auto">{session.user_name}</StatusChip>
							{/if}
						</div>

						{#if session.redacted}
							<p class="hub-np__title text-muted italic">Private listening</p>
						{:else}
							<p class="hub-np__title">{session.track_name}</p>
							<p class="hub-np__artist">{session.artist_name}</p>
						{/if}

						{#if session.device_name || session.audio_codec}
							<p class="hub-np__device">
								{#if session.device_name}
									<Laptop class="size-4 shrink-0" aria-hidden="true" />
									<span class="truncate">{session.device_name}</span>
								{/if}
								{#if session.audio_codec}
									<AudioQualityBadge
										codec={session.audio_codec}
										bitrate={session.bitrate}
										compact
									/>
								{/if}
							</p>
						{/if}

						{#if progress !== null && session.progress_ms != null && session.duration_ms != null}
							<div class="hub-np__progress">
								<span>{formatTime(session.progress_ms)}</span>
								<div class="hub-np__rail">
									<div class="hub-np__fill" style:width="{progress}%"></div>
								</div>
								<span>{formatTime(session.duration_ms)}</span>
							</div>
						{/if}
					</div>
				</div>
			{/each}
		</div>
	</section>
{/if}

<style>
	.hub-np {
		display: flex;
		flex-direction: column;
	}
	.hub-np__list {
		flex: 1;
		display: grid;
		align-content: center;
		gap: 0;
		padding: 0;
	}

	.hub-np__row {
		display: flex;
		gap: 1.25rem;
		padding: 1.125rem;
	}
	.hub-np__row + .hub-np__row {
		border-top: 1px solid var(--dn-line);
	}

	.hub-np__cover {
		flex: none;
		width: 8.5rem;
		height: 8.5rem;
		overflow: hidden;
		border-radius: 0.25rem;
	}
	.hub-np__row + .hub-np__row .hub-np__cover {
		width: 5rem;
		height: 5rem;
	}

	.hub-np__main {
		display: flex;
		min-width: 0;
		flex: 1;
		flex-direction: column;
		justify-content: center;
		gap: 0.125rem;
	}

	.hub-np__state {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.625rem;
		margin-bottom: 0.375rem;
	}
	.hub-np__source {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}

	.hub-np__title {
		overflow: hidden;
		font-size: 1.375rem;
		font-weight: 800;
		line-height: 1.2;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.hub-np__row + .hub-np__row .hub-np__title {
		font-size: 1rem;
	}
	.hub-np__artist {
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.9375rem;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.hub-np__device {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin-top: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--dn-muted);
	}

	.hub-np__progress {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		margin-top: 0.625rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-variant-numeric: tabular-nums;
	}
	.hub-np__rail {
		height: 6px;
		flex: 1;
		overflow: hidden;
		border-radius: 3px;
		background: var(--dn-line);
	}
	.hub-np__fill {
		height: 100%;
		border-radius: 3px;
		background: var(--dn-title);
		transition: width 1s linear;
	}
	.hub-np__row--paused .hub-np__fill {
		background: var(--dn-muted);
	}
	.hub-np__row--paused .hub-np__title,
	.hub-np__row--paused .hub-np__artist {
		color: var(--dn-muted);
	}

	@media (max-width: 639px) {
		.hub-np__state :global(.ml-auto) {
			margin-left: 0;
		}
		.hub-np__row {
			gap: 0.875rem;
			padding: 0.875rem;
		}
		.hub-np__cover,
		.hub-np__row + .hub-np__row .hub-np__cover {
			width: 5rem;
			height: 5rem;
		}
		.hub-np__title {
			font-size: 1.0625rem;
		}
		.hub-np__artist {
			font-size: 0.8125rem;
		}
	}
</style>
