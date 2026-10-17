<script lang="ts">
	import { onDestroy } from 'svelte';
	import type { PlaylistSummary } from '$lib/api/playlists';
	import { fetchPlaylist, isRedactedPlaylist } from '$lib/api/playlists';
	import { deletePlaylistV3 } from '$lib/queries/playlists/PlaylistV3Mutations.svelte';
	import { playlistTrackToQueueItem } from '$lib/player/queueHelpers';
	import { playerStore } from '$lib/stores/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { formatTotalDurationSec } from '$lib/utils/formatting';
	import { getSourceColor, getSourceLabel } from '$lib/utils/sources';
	import { withBasePath } from '$lib/utils/basePath';
	import { Play, Shuffle, Trash2, Tv, Lock, Globe } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import PlaylistMosaic from './PlaylistMosaic.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		playlist: PlaylistSummary;
		ondelete?: (playlistId: string) => void;
	}

	let { playlist, ondelete }: Props = $props();

	const deleteMutation = deletePlaylistV3();

	// Mutations (delete) are owner-only; admins may delete any playlist for cleanup.
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
			await deleteMutation.mutateAsync(playlist.id);
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

<Card
	variant="raised"
	padding="none"
	class="group relative w-full shrink-0"
	style={sourceColor
		? `background: color-mix(in srgb, ${sourceColor} 6%, var(--color-base-100)); border-left: 3px solid color-mix(in srgb, ${sourceColor} 50%, transparent);`
		: undefined}
>
	<a
		href={withBasePath(`/playlists/${playlist.id}`)}
		class="block relative z-0 transition-transform active:scale-95 focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-base-100 rounded-t-box"
		aria-label="Open {playlist.name}"
	>
		<figure class="aspect-square overflow-hidden relative">
			<div
				class="w-full h-full transition-transform duration-200 group-hover:scale-105 transform-gpu"
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
					class="absolute top-2 right-2 flex items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider shadow-md backdrop-blur-sm"
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
			<h3 class="text-sm font-semibold line-clamp-1">{playlist.name}</h3>
			<p class="text-xs text-base-content/60 mt-0.5">{subtitle}</p>
			{#if !playlist.is_owner && playlist.is_public}
				<div class="mt-1.5 flex items-center gap-1.5">
					<div class="avatar avatar-placeholder">
						<div class="w-4 h-4 rounded-full bg-accent text-accent-content">
							<span class="text-[8px] font-semibold leading-none">{ownerInitials}</span>
						</div>
					</div>
					<span class="text-[10px] text-base-content/60 line-clamp-1">
						Shared by {playlist.owner_name ?? 'someone'}
					</span>
				</div>
			{/if}
		</div>
	</a>

	<div class="flex items-center gap-1 px-3 pb-2.5 pt-1.5">
		<IconButton
			variant="primary"
			size="sm"
			icon={Play}
			label="Play {playlist.name}"
			class="[&_svg]:fill-current"
			loading={playLoading}
			disabled={!hasPlayableTracks}
			onclick={handlePlay}
		/>

		<IconButton
			size="sm"
			icon={Shuffle}
			label="Shuffle {playlist.name}"
			loading={shuffleLoading}
			disabled={!hasPlayableTracks}
			onclick={handleShuffle}
		/>

		{#if canDelete}
			<div class="ml-auto">
				<IconButton
					variant={deleteConfirming ? 'danger' : 'ghost'}
					size="sm"
					icon={Trash2}
					label={deleteConfirming ? `Confirm delete ${playlist.name}` : `Delete ${playlist.name}`}
					loading={deleting}
					onclick={handleDeleteClick}
				/>
			</div>
		{/if}
	</div>
</Card>
