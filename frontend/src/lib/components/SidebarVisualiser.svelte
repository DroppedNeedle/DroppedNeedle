<script lang="ts">
	import '$lib/components/now-playing.css';
	import { nowPlayingMerged } from '$lib/stores/nowPlayingMerged.svelte';

	const sourceHub: Record<string, string> = {
		jellyfin: '/library/jellyfin',
		navidrome: '/library/navidrome',
		plex: '/library/plex'
	};
</script>

{#if nowPlayingMerged.primarySession && !nowPlayingMerged.primarySession._isLocal}
	{@const session = nowPlayingMerged.primarySession}
	{@const isPaused = session.is_paused}
	{@const href = sourceHub[session.source ?? ''] ?? '#'}
	<li>
		<a
			{href}
			class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
			data-tip="{session.track_name} - {session.artist_name}"
			aria-label="Now playing: {session.track_name} by {session.artist_name}"
		>
			<div
				class="now-playing-bars now-playing-bars--sm {isPaused ? 'now-playing-bars--paused' : ''}"
			>
				<span></span><span></span><span></span>
			</div>
			<span class="is-drawer-close:hidden truncate text-xs opacity-70">
				{session.track_name}
			</span>
		</a>
	</li>
{/if}
