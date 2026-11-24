<!--
	The top ten songs on the page's one paper panel, with Play all, which
	queues the songs the library holds a track for, in rank order.
-->
<script lang="ts">
	import { Play } from 'lucide-svelte';
	import { getApiUrl } from '$lib/api/api-utils';
	import { albumArtworkPath, coverPath } from '$lib/api/covers';
	import type { QueueItem } from '$lib/features/player/playback/types';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import type { YearReview } from '$lib/features/year/queries/year.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import PaperPanel from '$lib/ui/PaperPanel.svelte';
	import { count } from './yearFormat';

	type Song = YearReview['top_songs'][number];

	interface Props {
		songs: Song[];
	}

	let { songs }: Props = $props();

	const shown = $derived(songs.slice(0, 10));

	// The cover AlbumImage shows: the album's own art, else its release group's.
	function coverOf(song: Song): string | null {
		if (song.art_missing) return null;
		if (song.album_id && song.cover_available) {
			return getApiUrl(albumArtworkPath(song.album_id, undefined, 250));
		}
		if (song.release_group_mbid) {
			return getApiUrl(coverPath('release-group', song.release_group_mbid, 250));
		}
		return null;
	}

	function queueItem(song: Song & { id: string }): QueueItem {
		const albumId = song.album_id ?? '';
		return {
			trackSourceId: song.id,
			trackName: song.name,
			artistName: song.artist_name,
			trackNumber: 0,
			albumId,
			albumName: song.album_name ?? '',
			coverUrl: coverOf(song),
			coverRemoteUrl: null,
			sourceType: 'local',
			availableSources: ['local'],
			playedFrom: { kind: 'unknown' }
		};
	}

	const queue = $derived(
		shown.flatMap((song) => (song.id ? [queueItem({ ...song, id: song.id })] : []))
	);

	function playAll() {
		playerStore.playQueue(queue, 0, false);
	}
</script>

<section aria-label="Top songs">
	<PaperPanel>
		{#snippet head()}
			<div class="yr-songs__head">
				<h2 class="yr-songs__title">Top songs</h2>
				{#if queue.length > 0}
					<Button variant="paper" size="display-sm" icon={Play} onclick={playAll}>Play all</Button>
				{/if}
			</div>
		{/snippet}
		<ol class="yr-songs__list">
			{#each shown as song, i (i)}
				<li class="yr-songs__row">
					<span class="yr-songs__rank" aria-hidden="true">{i + 1}</span>
					<span class="yr-songs__cover">
						<AlbumImage
							mbid={song.release_group_mbid ?? ''}
							albumId={song.album_id ?? undefined}
							available={song.cover_available}
							artMissing={song.art_missing || (!song.album_id && !song.release_group_mbid)}
							alt={song.name}
							size="full"
							requestSize={250}
							rounded="sm"
							className="h-full w-full"
						/>
					</span>
					<span class="yr-songs__words">
						<span class="yr-songs__name">{song.name}</span>
						<span class="yr-songs__artist">{song.artist_name}</span>
					</span>
					<span class="yr-songs__plays">
						{count(song.plays)}<span class="sr-only"> {song.plays === 1 ? 'play' : 'plays'}</span>
					</span>
				</li>
			{/each}
		</ol>
	</PaperPanel>
</section>

<style>
	.yr-songs__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		padding: 1rem 1.25rem;
	}
	.yr-songs__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 1.875rem;
		line-height: 1;
		text-transform: uppercase;
		font-synthesis: none;
	}

	.yr-songs__list {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		column-gap: 2.5rem;
		padding: 0.5rem 1.25rem 1rem;
		list-style: none;
	}
	@media (min-width: 1024px) {
		.yr-songs__list {
			grid-template-columns: repeat(2, minmax(0, 1fr));
			grid-template-rows: repeat(5, auto);
			grid-auto-flow: column;
		}
	}

	.yr-songs__row {
		display: flex;
		align-items: center;
		gap: 0.875rem;
		min-width: 0;
		padding-block: 0.625rem;
		border-bottom: 1px solid var(--dn-paper-border);
	}
	.yr-songs__rank {
		flex: none;
		width: 1.75rem;
		font-family: var(--font-display);
		font-size: 1.5rem;
		line-height: 1;
		text-align: right;
		font-variant-numeric: tabular-nums;
		font-synthesis: none;
	}
	.yr-songs__cover {
		position: relative;
		flex: none;
		width: 2.75rem;
		height: 2.75rem;
	}
	.yr-songs__words {
		display: flex;
		flex: 1;
		flex-direction: column;
		min-width: 0;
	}
	.yr-songs__name {
		overflow: hidden;
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.yr-songs__artist {
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.12em;
		text-overflow: ellipsis;
		text-transform: uppercase;
		white-space: nowrap;
	}
	.yr-songs__plays {
		flex: none;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 700;
		font-variant-numeric: tabular-nums;
	}
</style>
