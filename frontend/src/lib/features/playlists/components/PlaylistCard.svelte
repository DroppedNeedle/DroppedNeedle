<!--
	One playlist on the Playlists page: the mosaic at the left, the name in
	Anton, one mono line of counts, the owner on a shared one, and square
	Play, Shuffle and Delete buttons at the bottom right. The whole card opens
	the playlist; the buttons stay separate targets. An imported playlist
	carries its source as a tab on the card's top edge, in the source's
	colour; your own carry a globe or a lock. A list on rotation carries "On
	rotation", "Kept on" or "Always on" and one line saying how it is made.
-->
<script lang="ts">
	import { onDestroy } from 'svelte';
	import type { PlaylistSummary } from '$lib/api/playlists';
	import { fetchPlaylist, isRedactedPlaylist } from '$lib/api/playlists';
	import { createDeletePlaylistV3Mutation } from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import {
		playlistTrackToQueueItem,
		withPlayedFrom
	} from '$lib/features/player/playback/queueHelpers';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { formatTotalDurationSec } from '$lib/utils/formatting';
	import { getSourceColor, getSourceLabel, playlistSourceType } from '$lib/utils/sources';
	import { ideaRule } from './rotationIdeas';
	import { withBasePath } from '$lib/utils/basePath';
	import { Play, Shuffle, Trash2, Lock, Globe, Users } from 'lucide-svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import PlaylistMosaic from '$lib/media/PlaylistMosaic.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		playlist: PlaylistSummary;
		ondelete?: (playlistId: string) => void;
	}

	let { playlist, ondelete }: Props = $props();

	const deleteMutation = createDeletePlaylistV3Mutation();

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

	let sourceType = $derived(playlistSourceType(playlist));
	let sourceColor = $derived(sourceType ? getSourceColor(sourceType) : null);
	let sourceLabel = $derived(sourceType ? getSourceLabel(sourceType) : null);

	// A list on rotation says how it is made, under a tab of its own.
	let rotation = $derived(playlist.rotation);
	let rotationTab = $derived(
		rotation
			? rotation.always_on
				? 'Always on'
				: rotation.pinned
					? 'Kept on'
					: 'On rotation'
			: null
	);

	let durationText = $derived(
		playlist.total_duration ? formatTotalDurationSec(playlist.total_duration) : ''
	);

	let visibility = $derived(
		playlist.is_public
			? 'Public playlist'
			: playlist.is_shared
				? 'Shared playlist'
				: 'Private playlist'
	);
	let subtitle = $derived(
		`${playlist.track_count} track${playlist.track_count === 1 ? '' : 's'}${durationText ? ` · ${durationText}` : ''}${sourceLabel ? ` · from ${sourceLabel}` : ''}`
	);

	let hasPlayableTracks = $derived(playlist.track_count > 0);

	async function start(shuffle: boolean) {
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
		playerStore.playQueue(withPlayedFrom(items, { kind: 'playlist', id: playlist.id }), 0, shuffle);
	}

	async function handlePlay(e: Event) {
		e.preventDefault();
		e.stopPropagation();
		if (playLoading || shuffleLoading || !hasPlayableTracks) return;
		playLoading = true;
		try {
			await start(false);
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
			await start(true);
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

<article
	class="pl-card"
	class:pl-card--sourced={sourceColor && !rotationTab}
	class:pl-card--tabbed={sourceColor || rotationTab}
	style:--pl-source={sourceColor}
>
	<div class="pl-card__art">
		<PlaylistMosaic
			coverUrls={playlist.cover_urls}
			customCoverUrl={playlist.custom_cover_url}
			size="w-full h-full"
			rounded="rounded-none"
		/>
	</div>
	<a
		href={withBasePath(`/playlists/${playlist.id}`)}
		class="pl-card__link"
		aria-label="Open {playlist.name}"
	>
		<div class="pl-card__text">
			<h3 class="pl-card__name">{playlist.name}</h3>
			{#if rotation}
				<p class="pl-card__rule">{ideaRule(rotation)}</p>
			{/if}
			<p class="pl-card__meta">
				{#if playlist.is_owner && !rotation}
					<span class="pl-card__mark" title={visibility} aria-label={visibility} role="img">
						{#if playlist.is_public}
							<Globe class="size-3.5" aria-hidden="true" />
						{:else if playlist.is_shared}
							<Users class="size-3.5" aria-hidden="true" />
						{:else}
							<Lock class="size-3.5" aria-hidden="true" />
						{/if}
					</span>
				{/if}
				<span class="truncate">{subtitle}</span>
			</p>
			{#if !playlist.is_owner && (playlist.owner_name || playlist.can_delete)}
				<p class="pl-card__owner">
					<span class="pl-card__initials" aria-hidden="true">{ownerInitials}</span>
					<span class="truncate">
						{playlist.owner_name ? `Shared by ${playlist.owner_name}` : 'No owner'}
					</span>
				</p>
			{/if}
		</div>
	</a>

	{#if rotationTab}
		<span
			class="pl-card__tab pl-card__tab--rotation"
			class:pl-card__tab--live={rotationTab === 'On rotation'}
		>
			{rotationTab}
		</span>
	{:else if sourceType && sourceLabel}
		<span class="pl-card__tab">
			{#if sourceType === 'jellyfin'}
				<JellyfinIcon class="size-3" />
			{:else if sourceType === 'navidrome'}
				<NavidromeIcon class="size-3" />
			{:else if sourceType === 'plex'}
				<PlexIcon class="size-3" />
			{/if}
			{sourceLabel}
		</span>
	{/if}

	<div class="pl-card__actions">
		<IconButton
			variant="primary"
			size="sm"
			shape="square"
			icon={Play}
			label="Play {playlist.name}"
			class="[&_svg]:fill-current"
			loading={playLoading}
			disabled={!hasPlayableTracks}
			onclick={handlePlay}
		/>
		<IconButton
			variant="outline"
			size="sm"
			shape="square"
			icon={Shuffle}
			label="Shuffle {playlist.name}"
			loading={shuffleLoading}
			disabled={!hasPlayableTracks}
			onclick={handleShuffle}
		/>
		{#if playlist.can_delete}
			<IconButton
				variant={deleteConfirming ? 'danger' : 'danger-ghost'}
				size="sm"
				shape="square"
				icon={Trash2}
				label={deleteConfirming ? `Confirm delete ${playlist.name}` : `Delete ${playlist.name}`}
				loading={deleting}
				onclick={handleDeleteClick}
			/>
		{/if}
	</div>
</article>

<style>
	.pl-card {
		--pl-edge: var(--dn-line);
		position: relative;
		display: grid;
		grid-template-columns: 6rem minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.5rem 1rem;
		min-width: 0;
		padding: 0.875rem;
		border: 1px solid var(--pl-edge);
		border-radius: var(--radius-box);
		background-color: var(--color-base-200);
		transition: border-color 150ms;
	}
	.pl-card--sourced {
		--pl-edge: color-mix(in oklab, var(--pl-source) 55%, var(--dn-line));
	}
	.pl-card:hover {
		--pl-edge: var(--dn-title);
	}

	/* the link fills the card, so a click anywhere but a button opens it */
	.pl-card__link {
		min-width: 0;
		border-radius: 0.25rem;
	}
	.pl-card__link::after {
		content: '';
		position: absolute;
		inset: 0;
		border-radius: var(--radius-box);
	}
	.pl-card__link:focus-visible {
		outline: none;
	}
	.pl-card__link:focus-visible::after {
		outline: 2px solid var(--dn-focus);
		outline-offset: 2px;
	}

	.pl-card__art {
		flex: none;
		width: 6rem;
		aspect-ratio: 1;
		overflow: hidden;
		border-radius: 0.375rem;
		box-shadow: 0 6px 16px -8px rgb(0 0 0 / 0.6);
	}

	.pl-card__text {
		display: flex;
		min-width: 0;
		flex-direction: column;
		gap: 0.375rem;
	}

	.pl-card__name {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		font-family: var(--font-display);
		font-size: 1.5rem;
		font-weight: 400;
		line-height: 1.05;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		overflow-wrap: anywhere;
		color: var(--color-base-content);
		font-synthesis: none;
	}

	.pl-card__meta,
	.pl-card__owner {
		display: flex;
		min-width: 0;
		align-items: center;
		gap: 0.5rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		white-space: nowrap;
	}
	.pl-card__meta {
		color: var(--dn-muted);
	}
	.pl-card__mark {
		display: grid;
		flex: none;
		place-items: center;
		color: var(--color-base-content);
	}

	.pl-card__initials {
		display: grid;
		flex: none;
		place-items: center;
		width: 1.375rem;
		height: 1.375rem;
		border-radius: 9999px;
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		font-size: 0.5625rem;
		letter-spacing: 0.02em;
	}

	/* the source as a small tab on the top edge, in its colour */
	.pl-card__tab {
		position: absolute;
		top: -1px;
		right: 1rem;
		display: inline-flex;
		align-items: center;
		gap: 0.3125rem;
		padding: 0.1875rem 0.5rem 0.25rem;
		border-radius: 0 0 0.375rem 0.375rem;
		background-color: color-mix(in oklab, var(--pl-source) 82%, #000);
		color: #fff;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		pointer-events: none;
	}

	/* a list on rotation: acid while it rotates, quiet while it stays */
	.pl-card__tab--rotation {
		background-color: var(--color-neutral);
		color: var(--dn-title);
	}
	.pl-card__tab--live {
		background-color: var(--dn-action);
		color: var(--dn-action-content);
	}

	.pl-card__rule {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		font-size: 0.8125rem;
		line-height: 1.35;
		color: var(--dn-muted);
	}

	.pl-card__actions {
		position: relative;
		z-index: 1;
		display: flex;
		align-self: end;
		gap: 0.375rem;
	}

	/* Phone: the mosaic down the left, the words and then the buttons beside it */
	@media (max-width: 479px) {
		.pl-card {
			grid-template-columns: 5rem minmax(0, 1fr);
			gap: 0.5rem 0.875rem;
			align-items: start;
			padding: 0.75rem;
		}
		.pl-card--tabbed {
			padding-top: 1.5rem;
		}
		.pl-card__art {
			grid-row: 1 / 3;
			width: 5rem;
		}
		.pl-card__link {
			padding-top: 0.25rem;
		}
		.pl-card__name {
			font-size: 1.3125rem;
			-webkit-line-clamp: 1;
			line-clamp: 1;
		}
		.pl-card__actions {
			grid-column: 2;
			justify-content: flex-end;
		}
	}

	@media (prefers-contrast: more) {
		.pl-card__meta {
			color: var(--color-base-content);
		}
	}
</style>
