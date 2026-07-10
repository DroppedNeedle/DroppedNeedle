<script lang="ts">
	import { onDestroy } from 'svelte';
	import type { PlaylistSummary } from '$lib/api/playlists';
	import { fetchPlaylist, deletePlaylist, isRedactedPlaylist } from '$lib/api/playlists';
	import { playlistTrackToQueueItem } from '$lib/player/queueHelpers';
	import { playerStore } from '$lib/stores/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { formatTotalDurationSec } from '$lib/utils/formatting';
	import { getSourceColor, getSourceLabel } from '$lib/utils/sources';
	import { Play, Shuffle, Trash2, Tv, Lock, Globe } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import PlaylistMosaic from './PlaylistMosaic.svelte';

	interface Props {
		playlist: PlaylistSummary;
		ondelete?: (playlistId: string) => void;
	}

	let { playlist, ondelete }: Props = $props();

	// Mutations (delete) are owner-only; admins may delete any playlist for cleanup (D4).
	let canDelete = $derived(playlist.is_owner || authStore.isAdmin);
	let ownerInitials = $derived(
		(playlist.owner_name ?? '')
			.trim()
			.split(/\s+/)
			.map((w) => w[0])
			.slice(0, 2)
			.join('')
			.toUpperCase() || '?'
	);

	let playLoading = $state(false);
	let shuffleLoading = $state(false);
	let deleteConfirming = $state(false);
	let deleting = $state(false);
	let confirmTimer: ReturnType<typeof setTimeout> | undefined;

	let sourceType = $derived(playlist.source_ref?.split(':')[0] ?? null);
	let sourceColor = $derived(sourceType ? getSourceColor(sourceType) : null);
	let sourceLabel = $derived(sourceType ? getSourceLabel(sourceType) : null);

	let durationText = $derived(
		playlist.total_duration ? formatTotalDurationSec(playlist.total_duration) : ''
	);

	let subtitle = $derived(
		`${playlist.track_count} track${playlist.track_count === 1 ? '' : 's'}${durationText ? ` - ${durationText}` : ''}${sourceLabel ? ` - from ${sourceLabel}` : ''}`
	);

	let hasPlayableTracks = $derived(playlist.track_count > 0);

	async function handlePlay(e: Event) {
		e.preventDefault();
		e.stopPropagation();
		if (playLoading || shuffleLoading || !hasPlayableTracks) return;
		playLoading = true;
		try {
			const detail = await fetchPlaylist(playlist.id);
			if (isRedactedPlaylist(detail)) return;
			const items = detail.tracks
				.map(playlistTrackToQueueItem)
				.filter((item): item is NonNullable<typeof item> => item !== null);
			if (items.length === 0) {
				toastStore.show({
					message: "This playlist doesn't have anything playable yet.",
					type: 'info'
				});
				return;
			}
			playerStore.playQueue(items, 0, false);
		} catch {
			toastStore.show({ message: "Couldn't load that playlist.", type: 'error' });
		} finally {
			playLoading = false;
		}
	}

	async function handleShuffle(e: Event) {
		e.preventDefault();
		e.stopPropagation();
		if (shuffleLoading || playLoading || !hasPlayableTracks) return;
		shuffleLoading = true;
		try {
			const detail = await fetchPlaylist(playlist.id);
			if (isRedactedPlaylist(detail)) return;
			const items = detail.tracks
				.map(playlistTrackToQueueItem)
				.filter((item): item is NonNullable<typeof item> => item !== null);
			if (items.length === 0) {
				toastStore.show({
					message: "This playlist doesn't have anything playable yet.",
					type: 'info'
				});
				return;
			}
			playerStore.playQueue(items, 0, true);
		} catch {
			toastStore.show({ message: "Couldn't load that playlist.", type: 'error' });
		} finally {
			shuffleLoading = false;
		}
	}

	function handleDeleteClick(e: Event) {
		e.preventDefault();
		e.stopPropagation();
		if (deleting) return;

		if (!deleteConfirming) {
			deleteConfirming = true;
			confirmTimer = setTimeout(() => {
				deleteConfirming = false;
			}, 3000);
			return;
		}

		void confirmDelete();
	}

	async function confirmDelete() {
		clearTimeout(confirmTimer);
		deleting = true;
		try {
			await deletePlaylist(playlist.id);
			toastStore.show({ message: 'Playlist deleted.', type: 'success' });
			ondelete?.(playlist.id);
		} catch {
			toastStore.show({ message: "Couldn't delete that playlist.", type: 'error' });
		} finally {
			deleting = false;
			deleteConfirming = false;
		}
	}

	onDestroy(() => {
		clearTimeout(confirmTimer);
	});
</script>

<div
	class="group relative flex w-full shrink-0 flex-col overflow-hidden rounded-2xl border border-base-content/8 bg-base-200/50 transition-colors duration-200 hover:border-primary/30"
	style={sourceColor
		? `background: color-mix(in srgb, ${sourceColor} 6%, oklch(from var(--color-base-200) l c h / 0.5)); border-left: 3px solid color-mix(in srgb, ${sourceColor} 50%, transparent);`
		: ''}
>
	<a
		href="/playlists/{playlist.id}"
		class="relative z-0 block rounded-t-2xl transition-transform focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-base-100 active:scale-95"
		aria-label="Open {playlist.name}"
	>
		<figure class="relative aspect-square overflow-hidden">
			<div
				class="h-full w-full transform-gpu transition-transform duration-200 group-hover:scale-105"
			>
				<PlaylistMosaic
					coverUrls={playlist.cover_urls}
					customCoverUrl={playlist.custom_cover_url}
					size="w-full h-full"
					rounded="none"
				/>
			</div>
			{#if sourceType}
				<div
					class="absolute top-2 right-2 flex items-center gap-1 rounded-full px-2 py-0.5 font-mono text-[10px] font-semibold uppercase tracking-wider shadow-md backdrop-blur-sm"
					style="background: color-mix(in srgb, {sourceColor} 85%, black); color: white;"
				>
					{#if sourceType === 'jellyfin'}
						<Tv class="h-3 w-3" />
					{:else if sourceType === 'navidrome'}
						<NavidromeIcon class="h-3 w-3" />
					{:else if sourceType === 'plex'}
						<PlexIcon class="h-3 w-3" />
					{/if}
					<span>{sourceLabel}</span>
				</div>
			{/if}
			{#if playlist.is_owner}
				<div
					class="absolute top-2 left-2 flex items-center justify-center rounded-full bg-base-100/80 p-1 shadow-sm backdrop-blur-sm"
					title={playlist.is_public ? 'Public playlist' : 'Private playlist'}
					aria-label={playlist.is_public ? 'Public playlist' : 'Private playlist'}
				>
					{#if playlist.is_public}
						<Globe class="h-3 w-3 text-success" />
					{:else}
						<Lock class="h-3 w-3 text-base-content/60" />
					{/if}
				</div>
			{/if}
		</figure>
		<div class="px-3 pt-3 pb-1">
			<h3 class="line-clamp-1 font-display text-sm font-semibold tracking-tight">
				{playlist.name}
			</h3>
			<p class="mt-0.5 text-xs text-base-content/60">{subtitle}</p>
			{#if !playlist.is_owner && playlist.is_public}
				<div class="mt-1.5 flex items-center gap-1.5">
					<div class="avatar avatar-placeholder">
						<div class="h-4 w-4 rounded-full bg-accent text-accent-content">
							<span class="text-[8px] font-semibold leading-none">{ownerInitials}</span>
						</div>
					</div>
					<span class="line-clamp-1 text-[10px] text-base-content/60">
						Shared by {playlist.owner_name ?? 'someone'}
					</span>
				</div>
			{/if}
		</div>
	</a>

	<div class="flex items-center gap-1 px-3 pt-1.5 pb-2.5">
		<button
			class="btn btn-circle btn-sm btn-primary border-none shadow-md transition-transform duration-150 hover:scale-110 active:scale-95"
			onclick={handlePlay}
			disabled={!hasPlayableTracks || playLoading}
			aria-label="Play {playlist.name}"
			title={hasPlayableTracks ? `Play ${playlist.name}` : 'No playable tracks'}
		>
			{#if playLoading}
				<span class="loading loading-spinner loading-xs"></span>
			{:else}
				<Play class="h-4 w-4 fill-current" />
			{/if}
		</button>

		<button
			class="btn btn-circle btn-sm btn-ghost text-base-content/50 transition-colors duration-150 hover:text-base-content"
			onclick={handleShuffle}
			disabled={!hasPlayableTracks || shuffleLoading}
			aria-label="Shuffle {playlist.name}"
			title={hasPlayableTracks ? `Shuffle ${playlist.name}` : 'No playable tracks'}
		>
			{#if shuffleLoading}
				<span class="loading loading-spinner loading-xs"></span>
			{:else}
				<Shuffle class="h-3.5 w-3.5" />
			{/if}
		</button>

		{#if canDelete}
			<div class="ml-auto">
				<button
					class="btn btn-circle btn-sm transition-all duration-150 {deleteConfirming
						? 'btn-error shadow-md animate-pulse'
						: 'btn-ghost text-base-content/50 hover:text-error'}"
					onclick={handleDeleteClick}
					disabled={deleting}
					aria-label={deleteConfirming
						? `Confirm delete ${playlist.name}`
						: `Delete ${playlist.name}`}
					title={deleteConfirming ? 'Click again to delete' : `Delete ${playlist.name}`}
				>
					{#if deleting}
						<span class="loading loading-spinner loading-xs"></span>
					{:else}
						<Trash2 class="h-3.5 w-3.5" />
					{/if}
				</button>
			</div>
		{/if}
	</div>
</div>
