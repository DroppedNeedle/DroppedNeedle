<script lang="ts">
	import './hub.css';
	import { Blend, MicVocal, TrendingUp } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import DiscoveryShelf from '$lib/features/remote-library/components/DiscoveryShelf.svelte';
	import DiscoveryTrackTable from '$lib/features/remote-library/components/DiscoveryTrackTable.svelte';
	import HubShelf from '$lib/features/remote-library/components/HubShelf.svelte';
	import { reveal } from '$lib/actions/reveal';
	import {
		getRemoteInfoArtistQuery,
		getRemoteSimilarQuery,
		getRemoteTopQuery
	} from '$lib/features/remote-library/queries/RemoteQueries.svelte';
	import type { RemoteSourceDescriptor } from '$lib/features/remote-library/queries/remoteSources';
	import type { RemoteArtist, RemoteTrack } from '$lib/features/remote-library/queries/types';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { formatDurationSec as formatDuration } from '$lib/utils/formatting';
	import { toDiscoveryTrack } from './remoteTracks';

	interface Props {
		remote: RemoteSourceDescriptor;
		/** The favourite artist the shelves are about. */
		artist: RemoteArtist | null;
		/** The favourite track the similar songs follow. */
		track: RemoteTrack | null;
	}

	let { remote, artist, track }: Props = $props();

	const artistName = $derived(artist?.name ?? '');
	const topQuery = getRemoteTopQuery(
		() => remote.source,
		() => artistName,
		() => 20
	);
	const similarQuery = getRemoteSimilarQuery(
		() => remote.source,
		() => track?.id ?? '',
		() => ({ limit: 20 })
	);
	const infoQuery = getRemoteInfoArtistQuery(
		() => remote.source,
		() => artist?.id ?? ''
	);

	const topSongs = $derived(topQuery.data?.items ?? []);
	const similarSongs = $derived(similarQuery.data?.items ?? []);
	const info = $derived(infoQuery.data ?? null);

	function play(tracks: RemoteTrack[], startIndex = 0) {
		if (tracks.length === 0) return;
		playerStore.playQueue(remote.queue(tracks), startIndex);
	}
</script>

{#if artistName || track}
	<div use:reveal class="hub-pair">
		{#if artistName}
			<DiscoveryShelf
				title="Top Songs for {artistName}"
				icon={TrendingUp}
				loading={topQuery.isFetching}
				empty={!topQuery.isFetching && topSongs.length === 0}
				emptyMessage={topQuery.isError
					? "Couldn't load top songs."
					: 'Connect Last.fm to see top songs.'}
				onrefresh={() => void topQuery.refetch()}
				onplayall={() => play(topSongs)}
			>
				<DiscoveryTrackTable
					tracks={topSongs.map(toDiscoveryTrack)}
					onplay={(i) => play(topSongs, i)}
					{formatDuration}
				/>
			</DiscoveryShelf>
		{/if}

		{#if track}
			<DiscoveryShelf
				title="Similar Songs"
				icon={Blend}
				loading={similarQuery.isFetching}
				empty={!similarQuery.isFetching && similarSongs.length === 0}
				emptyMessage={similarQuery.isError
					? "Couldn't load similar songs."
					: 'Connect Last.fm to see similar songs.'}
				onrefresh={() => void similarQuery.refetch()}
				onplayall={() => play(similarSongs)}
			>
				<DiscoveryTrackTable
					tracks={similarSongs.map(toDiscoveryTrack)}
					onplay={(i) => play(similarSongs, i)}
					{formatDuration}
				/>
			</DiscoveryShelf>
		{/if}
	</div>
{/if}

{#if info}
	<div use:reveal>
		<HubShelf title="About {artistName}" icon={MicVocal} loading={infoQuery.isFetching}>
			<div class="about">
				{#if info.image_url}
					<div class="about__photo">
						<AlbumImage
							customUrl={info.image_url}
							alt={artistName}
							size="full"
							requestSize={250}
							rounded="full"
							className="h-full w-full"
						/>
					</div>
				{/if}
				<div class="min-w-0 space-y-4">
					{#if info.biography}
						<p class="about__bio">{info.biography}</p>
					{/if}
					{#if info.similar_artists.length > 0}
						<div>
							<p class="hub-sub mb-2">Similar Artists</p>
							<ul class="hub-pills">
								{#each info.similar_artists.slice(0, 8) as similar (similar.id)}
									<li class="about__chip">{similar.name}</li>
								{/each}
							</ul>
						</div>
					{/if}
				</div>
			</div>
		</HubShelf>
	</div>
{/if}

<style>
	.about {
		display: flex;
		align-items: flex-start;
		gap: 1.5rem;
		padding: 0.25rem;
	}
	.about__photo {
		flex: none;
		width: 7.5rem;
		height: 7.5rem;
		overflow: hidden;
		border-radius: 999px;
	}
	.about__bio {
		display: -webkit-box;
		max-width: 75ch;
		overflow: hidden;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 4;
		line-clamp: 4;
		line-height: 1.6;
	}
	.about__chip {
		padding: 0.25rem 0.75rem;
		border: 1px solid var(--dn-line);
		border-radius: 999px;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.06em;
	}
	@media (max-width: 639px) {
		.about {
			flex-direction: column;
			gap: 1rem;
		}
		.about__photo {
			width: 5.5rem;
			height: 5.5rem;
		}
	}
</style>
