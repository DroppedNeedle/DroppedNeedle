<!--
	"Made for you" at the top of Playlists: one row of covers for the lists
	the app makes and keeps fresh (Your Weekly Mix, then the lists on
	rotation), each with one line on when it changes, and a dashed tile at
	the end naming the rotation ideas waiting their turn and those skipped
	for having too few songs. Lists on rotation hide while they are turned
	off; the shelf shows nothing while it has nothing.
-->
<script lang="ts">
	import type { PlaylistSummary } from '$lib/api/playlists';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { RefreshCw } from 'lucide-svelte';
	import HorizontalCarousel from '$lib/ui/HorizontalCarousel.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import PlaylistMosaic from '$lib/media/PlaylistMosaic.svelte';
	import { getRotationQuery } from '$lib/features/playlists/queries/rotation.svelte';
	import PlaylistPlayButtons from './PlaylistPlayButtons.svelte';
	import { MADE_FOR_YOU_KINDS, type MadeForYouKind } from './madeForYou';
	import { changeWeekday, ideaName } from './rotationIdeas';

	interface Props {
		/** The caller's own lists the app makes, any order. */
		lists: (PlaylistSummary & { kind: MadeForYouKind })[];
	}

	let { lists }: Props = $props();

	const rotationQuery = getRotationQuery(() => authStore.isAuthenticated);

	const status = $derived(rotationQuery.data);
	// Lists on rotation show while their status loads or fails; only "off" hides them.
	const rotationOn = $derived(status?.enabled !== false);

	/** Each kind's lists in shelf order, empty kinds left out. */
	const groups = $derived(
		MADE_FOR_YOU_KINDS.map((kind) => ({
			kind,
			lists: kind === 'rotation' && !rotationOn ? [] : lists.filter((l) => l.kind === kind)
		})).filter((group) => group.lists.length > 0)
	);
	const count = $derived(groups.reduce((sum, group) => sum + group.lists.length, 0));

	const waiting = $derived((status?.waiting ?? []).map(ideaName));
	const thin = $derived((status?.thin ?? []).map(ideaName));

	/** The one line under a tile: when the list changes next. */
	function freshness(playlist: PlaylistSummary & { kind: MadeForYouKind }): string {
		switch (playlist.kind) {
			case 'weekly_mix':
				return 'Each week';
			case 'rotation': {
				const next = playlist.rotation?.next_change_at ?? status?.next_change_at;
				return next ? `Changes ${changeWeekday(next)}` : 'Each week';
			}
		}
	}
</script>

{#if count > 0}
	<section aria-label="Made for you">
		<SectionHead title="Made for you">
			{#snippet chips()}
				<span class="mfy-count">{count}</span>
			{/snippet}
		</SectionHead>
		<HorizontalCarousel>
			{#each groups as group, i (group.kind)}
				{#if i > 0}
					<div class="mfy-rule" aria-hidden="true"></div>
				{/if}
				{#each group.lists as playlist (playlist.id)}
					<MediaTile
						title={playlist.name}
						href={withBasePath(`/playlists/${playlist.id}`)}
						class="mfy-tile"
					>
						{#snippet art()}
							<PlaylistMosaic
								playlistId={playlist.id}
								coverUrls={playlist.cover_urls}
								customCoverUrl={playlist.custom_cover_url}
								size="w-full h-full"
								rounded="rounded-lg"
							/>
						{/snippet}
						{#snippet meta()}
							<span class="mfy-when">{freshness(playlist)}</span>
						{/snippet}
						{#snippet overlay()}
							<div class="mfy-play">
								<PlaylistPlayButtons {playlist} />
							</div>
						{/snippet}
					</MediaTile>
				{/each}
				{#if group.kind === 'rotation' && (waiting.length > 0 || thin.length > 0)}
					<div class="mfy-tile mfy-ideas">
						<div class="mfy-ideas__box">
							<RefreshCw class="size-5" aria-hidden="true" />
							{#if waiting.length > 0}
								<p class="mfy-ideas__head">{waiting.length} waiting</p>
								<p class="mfy-ideas__names">{waiting.join(' · ')}</p>
							{/if}
						</div>
						<p class="mfy-ideas__label">Ideas</p>
						{#if thin.length > 0}
							<p class="mfy-ideas__thin">Skipped while thin: {thin.join(', ')}</p>
						{/if}
					</div>
				{/if}
			{/each}
		</HorizontalCarousel>
	</section>
{/if}

<style>
	.mfy-count,
	.mfy-when {
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.mfy-count {
		font-size: 0.6875rem;
	}

	:global(.mfy-tile) {
		flex: none;
		width: 8.25rem;
	}
	@media (max-width: 639px) {
		:global(.mfy-tile) {
			width: 7rem;
		}
	}

	/* between the app's mixes and the lists on rotation */
	.mfy-rule {
		flex: none;
		align-self: flex-start;
		width: 1px;
		height: 8.25rem;
		background-color: var(--dn-line);
	}
	@media (max-width: 639px) {
		.mfy-rule {
			height: 7rem;
		}
	}

	/* Play and Shuffle over the cover's corner, on hover and on focus */
	.mfy-play {
		position: absolute;
		left: 0.5rem;
		bottom: 0.5rem;
		opacity: 0;
		transition: opacity 150ms;
	}
	:global(.mfy-tile:hover) .mfy-play,
	:global(.mfy-tile:focus-within) .mfy-play {
		opacity: 1;
	}
	@media (hover: none) {
		.mfy-play {
			display: none;
		}
	}

	.mfy-ideas__box {
		display: flex;
		aspect-ratio: 1;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.375rem;
		overflow: hidden;
		padding: 0.625rem;
		border: 2px dashed var(--dn-edge);
		border-radius: 0.5rem;
		color: var(--dn-muted);
		text-align: center;
	}
	.mfy-ideas__head {
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}
	.mfy-ideas__names,
	.mfy-ideas__thin {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		font-size: 0.75rem;
		line-height: 1.3;
	}
	.mfy-ideas__names {
		color: var(--color-base-content);
	}
	.mfy-ideas__thin {
		margin-top: 0.25rem;
		padding-inline: 0.25rem;
		color: var(--dn-muted);
	}
	.mfy-ideas__label {
		padding: 0.625rem 0.25rem 0;
		font-size: 0.875rem;
		font-weight: 700;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
</style>
