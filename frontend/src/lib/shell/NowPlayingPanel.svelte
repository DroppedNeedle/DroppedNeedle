<script lang="ts">
	/*
	 * NowPlayingPanel — the ultrawide right-hand panel: current artwork + queue
	 * at a glance, so very wide screens use their space instead of wasting it.
	 * Rendered only ≥2xl (CSS-gated); state lives in playerUi.sidePanelOpen.
	 */
	import { playerStore } from '$lib/stores/player.svelte';
	import { playerUi } from '$lib/stores/playerUi.svelte';
	import { deckFocus } from '$lib/stores/deckFocus.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import QueueList from '$lib/components/player/QueueList.svelte';
	import AudioQualityBadge from '$lib/components/AudioQualityBadge.svelte';
	import { X, Disc3, Maximize2 } from 'lucide-svelte';

	let coverImgError = $state(false);
	let lastCoverKey = '';

	const visible = $derived(
		playerUi.sidePanelOpen &&
			playerStore.isPlayerVisible &&
			playerStore.nowPlaying !== null &&
			!deckFocus.inView
	);

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

{#if visible && playerStore.nowPlaying}
	<aside class="dn-np-panel hidden 2xl:flex" aria-label="Now playing panel">
		<div class="flex items-center justify-between px-4 pt-4 pb-2">
			<p class="text-[0.65rem] font-bold uppercase tracking-[0.16em] opacity-50">Now playing</p>
			<div class="flex items-center gap-1">
				<button
					class="btn btn-ghost btn-xs btn-circle opacity-60 hover:opacity-100"
					onclick={() => playerUi.openStage()}
					aria-label="Open full-screen player"
				>
					<Maximize2 class="h-3.5 w-3.5" />
				</button>
				<button
					class="btn btn-ghost btn-xs btn-circle opacity-60 hover:opacity-100"
					onclick={() => playerUi.toggleSidePanel()}
					aria-label="Close now playing panel"
				>
					<X class="h-3.5 w-3.5" />
				</button>
			</div>
		</div>

		<div class="px-4 pb-3">
			<button
				class="dn-np-panel__art group relative block w-full"
				onclick={() => playerUi.openStage()}
				aria-label="Open full-screen player"
			>
				{#if nowPlayingCoverUrl && !coverImgError}
					<img
						src={nowPlayingCoverUrl}
						alt={playerStore.nowPlaying.albumName}
						class="aspect-square w-full rounded-xl object-cover shadow-lg ring-1 ring-base-content/10"
						onerror={() => (coverImgError = true)}
					/>
				{:else}
					<div
						class="flex aspect-square w-full items-center justify-center rounded-xl bg-base-200 shadow-lg"
					>
						<Disc3 class="h-14 w-14 text-base-content/15" />
					</div>
				{/if}
			</button>
			<p class="mt-3 truncate text-sm font-semibold">
				{playerStore.nowPlaying.trackName || playerStore.nowPlaying.albumName}
			</p>
			<p class="truncate text-xs opacity-60">{playerStore.nowPlaying.artistName}</p>
			{#if playerStore.nowPlaying.format}
				<div class="mt-1.5">
					<AudioQualityBadge codec={playerStore.nowPlaying.format} compact />
				</div>
			{/if}
		</div>

		<div class="min-h-0 flex-1 overflow-y-auto border-t border-base-content/8">
			<QueueList active={visible} />
		</div>
	</aside>
{/if}

<style>
	.dn-np-panel {
		flex-direction: column;
		width: 21rem;
		flex-shrink: 0;
		position: sticky;
		top: 4rem;
		/* the fixed player dock owns the bottom strip while the panel is visible */
		height: calc(100dvh - 4rem - var(--ms-player-height));
		border-left: 1px solid var(--dn-hairline);
		background: oklch(from var(--color-base-200) l c h / 0.45);
	}
</style>
