<script lang="ts">
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import DiscoveryShelf from '$lib/components/DiscoveryShelf.svelte';
	import DiscoveryTrackTable from '$lib/components/DiscoveryTrackTable.svelte';
	import HubShelf from '$lib/components/HubShelf.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { reveal } from '$lib/actions/reveal';
	import {
		getRemoteInfoArtistQuery,
		getRemoteSimilarQuery,
		getRemoteTopQuery
	} from '$lib/queries/remotes/RemoteQueries.svelte';
	import type { RemoteSourceDescriptor } from '$lib/queries/remotes/remoteSources';
	import type { RemoteArtist, RemoteTrack } from '$lib/queries/remotes/types';
	import { playerStore } from '$lib/stores/player.svelte';
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

{#if artistName}
	<div use:reveal>
		<DiscoveryShelf
			title="Top Songs for {artistName}"
			loading={topQuery.isFetching}
			empty={!topQuery.isFetching && topSongs.length === 0}
			emptyMessage={topQuery.isError
				? "Couldn't load top songs."
				: 'Connect Last.fm to see top songs.'}
			onrefresh={() => void topQuery.refetch()}
		>
			{#if topSongs.length > 0}
				<div class="flex items-center gap-2 mb-3">
					<Button variant="primary" size="sm" onclick={() => play(topSongs)}>Play all</Button>
				</div>
				<DiscoveryTrackTable
					tracks={topSongs.map(toDiscoveryTrack)}
					onplay={(i) => play(topSongs, i)}
					{formatDuration}
				/>
			{/if}
		</DiscoveryShelf>
	</div>
{/if}

{#if track}
	<div use:reveal>
		<DiscoveryShelf
			title="Similar Songs"
			loading={similarQuery.isFetching}
			empty={!similarQuery.isFetching && similarSongs.length === 0}
			emptyMessage={similarQuery.isError
				? "Couldn't load similar songs."
				: 'Connect Last.fm to see similar songs.'}
			onrefresh={() => void similarQuery.refetch()}
		>
			{#if similarSongs.length > 0}
				<div class="flex items-center gap-2 mb-3">
					<Button variant="primary" size="sm" onclick={() => play(similarSongs)}>Play all</Button>
				</div>
				<DiscoveryTrackTable
					tracks={similarSongs.map(toDiscoveryTrack)}
					onplay={(i) => play(similarSongs, i)}
					{formatDuration}
				/>
			{/if}
		</DiscoveryShelf>
	</div>
{/if}

{#if info}
	<div use:reveal>
		<HubShelf title="About {artistName}" loading={infoQuery.isFetching}>
			<div class="flex gap-4 items-start">
				{#if info.image_url}
					<div class="h-24 w-24 shrink-0">
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
				<div class="space-y-2">
					{#if info.biography}
						<p class="text-sm text-base-content/70 line-clamp-4">{info.biography}</p>
					{/if}
					{#if info.similar_artists.length > 0}
						<div>
							<p class="text-xs font-semibold text-base-content/50 mb-1">Similar Artists</p>
							<div class="flex flex-wrap gap-1">
								{#each info.similar_artists.slice(0, 8) as similar (similar.id)}
									<span class="badge badge-sm badge-outline">{similar.name}</span>
								{/each}
							</div>
						</div>
					{/if}
				</div>
			</div>
		</HubShelf>
	</div>
{/if}
