<!--
	One playlist in the Playlists page's lists: a small cover, the name, the
	song count, where it came from when it was imported, the owner on a
	shared one, and a warning only while its Spotify sync needs a look. The
	whole row opens the playlist. Play and Shuffle show on hover and on
	keyboard focus; a touch screen leaves them to the playlist's page.
-->
<script lang="ts">
	import type { PlaylistSummary } from '$lib/api/playlists';
	import { getPlaylistSyncQuery } from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
	import { getSourceColor, getSourceLabel, playlistSourceType } from '$lib/utils/sources';
	import { withBasePath } from '$lib/utils/basePath';
	import { TriangleAlert } from 'lucide-svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import SpotifyIcon from '$lib/media/icons/SpotifyIcon.svelte';
	import PlaylistMosaic from '$lib/media/PlaylistMosaic.svelte';
	import PlaylistPlayButtons from './PlaylistPlayButtons.svelte';
	import { syncLine } from './syncStates';

	interface Props {
		playlist: PlaylistSummary;
	}

	let { playlist }: Props = $props();

	let sourceType = $derived(playlistSourceType(playlist));
	let sourceLabel = $derived(sourceType ? getSourceLabel(sourceType) : null);
	let sourceColor = $derived(sourceType ? getSourceColor(sourceType) : null);

	// Only the owner's Spotify lists have a sync to look at.
	const syncQuery = getPlaylistSyncQuery(
		() => playlist.id,
		() => playlist.is_owner && sourceType === 'spotify'
	);
	let syncWarning = $derived.by(() => {
		const sync = syncQuery.data;
		if (!sync?.enabled) return null;
		const line = syncLine(sync);
		if (line?.tone !== 'warn') return null;
		return {
			text: line.text,
			short: sync.failure || sync.spotify_off ? 'Sync stopped' : 'Sync held'
		};
	});

	let songs = $derived(`${playlist.track_count} song${playlist.track_count === 1 ? '' : 's'}`);
	let owner = $derived(
		playlist.is_owner ? null : playlist.owner_name ? `By ${playlist.owner_name}` : 'No owner'
	);
</script>

<article class="pl-row">
	<div class="pl-row__art">
		<PlaylistMosaic
			playlistId={playlist.id}
			coverUrls={playlist.cover_urls}
			customCoverUrl={playlist.custom_cover_url}
			size="w-full h-full"
			rounded="rounded-none"
		/>
	</div>
	<a href={withBasePath(`/playlists/${playlist.id}`)} class="pl-row__link">
		<h3 class="pl-row__name">{playlist.name}</h3>
		<p class="pl-row__meta">
			<span>{songs}</span>
			{#if sourceType && sourceLabel}
				<span class="pl-row__source" style:--pl-source={sourceColor}>
					{#if sourceType === 'spotify'}
						<SpotifyIcon class="size-3" />
					{:else if sourceType === 'jellyfin'}
						<JellyfinIcon class="size-3" />
					{:else if sourceType === 'navidrome'}
						<NavidromeIcon class="size-3" />
					{:else if sourceType === 'plex'}
						<PlexIcon class="size-3" />
					{/if}
					{sourceLabel}
				</span>
			{/if}
			{#if owner}
				<span>{owner}</span>
			{/if}
			{#if syncWarning}
				<span class="pl-row__warn" title={syncWarning.text}>
					<TriangleAlert class="size-3" aria-hidden="true" />
					<span>{syncWarning.short}</span>
					<span class="sr-only">: {syncWarning.text}</span>
				</span>
			{/if}
		</p>
	</a>
	<div class="pl-row__actions">
		<PlaylistPlayButtons {playlist} />
	</div>
</article>

<style>
	.pl-row {
		position: relative;
		display: grid;
		grid-template-columns: 3.25rem minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.75rem;
		min-width: 0;
		padding: 0.375rem;
		border: 1px solid transparent;
		border-radius: var(--radius-box);
		transition:
			background-color 150ms,
			border-color 150ms;
	}
	.pl-row:hover,
	.pl-row:focus-within {
		border-color: var(--dn-line);
		background-color: var(--color-base-200);
	}

	.pl-row__art {
		width: 3.25rem;
		aspect-ratio: 1;
		overflow: hidden;
		border-radius: 0.375rem;
	}

	/* the link fills the row, so a click anywhere but a button opens it */
	.pl-row__link {
		display: flex;
		min-width: 0;
		flex-direction: column;
		gap: 0.25rem;
	}
	.pl-row__link::after {
		content: '';
		position: absolute;
		inset: 0;
		border-radius: var(--radius-box);
	}
	.pl-row__link:focus-visible {
		outline: none;
	}
	.pl-row__link:focus-visible::after {
		outline: 2px solid var(--dn-focus);
		outline-offset: 2px;
	}

	.pl-row__name {
		overflow: hidden;
		font-size: 0.9375rem;
		font-weight: 600;
		line-height: 1.25;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--color-base-content);
	}

	.pl-row__meta {
		display: flex;
		min-width: 0;
		align-items: center;
		gap: 0.625rem;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--dn-muted);
	}
	.pl-row__source,
	.pl-row__warn {
		display: inline-flex;
		flex: none;
		align-items: center;
		gap: 0.25rem;
	}
	.pl-row__source {
		color: var(--pl-source);
	}
	.pl-row__warn {
		color: var(--color-warning);
	}

	.pl-row__actions {
		position: relative;
		z-index: 1;
		opacity: 0;
		transition: opacity 150ms;
	}
	.pl-row:hover .pl-row__actions,
	.pl-row:focus-within .pl-row__actions {
		opacity: 1;
	}
	/* a touch screen opens the playlist, where Play lives */
	@media (hover: none) {
		.pl-row {
			grid-template-columns: 3.25rem minmax(0, 1fr);
		}
		.pl-row__actions {
			display: none;
		}
	}

	@media (prefers-contrast: more) {
		.pl-row__meta {
			color: var(--color-base-content);
		}
	}
</style>
