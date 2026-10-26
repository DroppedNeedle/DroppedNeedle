<script lang="ts">
	import DiscoveryShelf from '$lib/components/DiscoveryShelf.svelte';
	import DiscoveryTrackTable from '$lib/components/DiscoveryTrackTable.svelte';
	import DiscoveryZone from '$lib/components/DiscoveryZone.svelte';
	import GenrePillFilter from '$lib/components/GenrePillFilter.svelte';
	import GenreSongsBrowser, { type BrowseTrack } from '$lib/components/GenreSongsBrowser.svelte';
	import Button from '$lib/ui/Button.svelte';
	import {
		fetchRemoteGenreSongs,
		getRemoteDiscoveryQuery,
		getRemoteMixQuery,
		getRemoteRandomQuery
	} from '$lib/queries/remotes/RemoteQueries.svelte';
	import type { RemoteSourceDescriptor } from '$lib/queries/remotes/remoteSources';
	import type { RemoteAlbum, RemoteTrack } from '$lib/queries/remotes/types';
	import { playerStore } from '$lib/stores/player.svelte';
	import { formatDurationSec as formatDuration } from '$lib/utils/formatting';
	import { SvelteMap } from 'svelte/reactivity';
	import AlbumCarousel from './AlbumCarousel.svelte';
	import { toBrowseTrack, toDiscoveryTrack } from './remoteTracks';

	interface Props {
		remote: RemoteSourceDescriptor;
		genres: string[];
		onopen: (album: RemoteAlbum) => void;
	}

	let { remote, genres, onopen }: Props = $props();
	const caps = $derived(remote.capabilities);

	let mixGenre = $state('');
	let randomGenre = $state<string | undefined>(undefined);

	const mixQuery = getRemoteMixQuery(
		() => remote.source,
		() => mixGenre,
		() => ({ kind: 'genre', limit: 30 }),
		() => caps.mix
	);
	const randomQuery = getRemoteRandomQuery(
		() => remote.source,
		() => ({ limit: 20, genre: randomGenre }),
		() => caps.random
	);
	const discoveryQuery = getRemoteDiscoveryQuery(
		() => remote.source,
		() => 10,
		() => caps.recommendations
	);

	const mixTracks = $derived(mixQuery.data?.items ?? []);
	const randomTracks = $derived(randomQuery.data?.items ?? []);
	const recommended = $derived(discoveryQuery.data?.hubs ?? []);

	function play(tracks: RemoteTrack[], startIndex = 0, shuffle = false) {
		if (tracks.length === 0) return;
		playerStore.playQueue(remote.queue(tracks), startIndex, shuffle);
	}

	// The genre browser hands back its own rows; keep the full tracks to queue them.
	const genreTracks = new SvelteMap<string, RemoteTrack>();
	const GENRE_TRACKS_MAX = 500;

	async function fetchGenreSongs(chosen: string[], limit: number, offset: number) {
		const tracks = await fetchRemoteGenreSongs(remote.source, chosen, caps.multiGenre, {
			limit,
			offset
		});
		if (genreTracks.size > GENRE_TRACKS_MAX) genreTracks.clear();
		for (const t of tracks) genreTracks.set(t.id, t);
		return tracks.map(toBrowseTrack);
	}

	function genreQueue(tracks: BrowseTrack[]) {
		return remote.queue(
			tracks.map(
				(t) =>
					genreTracks.get(t.id) ?? {
						source: remote.source,
						id: t.id,
						title: t.title,
						artist_name: t.artist_name,
						album_name: t.album_name,
						duration_secs: t.duration_seconds,
						image_url: t.image_url ?? null
					}
			)
		);
	}
</script>

{#if !caps.mix || genres.length > 0 || mixTracks.length > 0}
	<DiscoveryZone>
		{#if caps.mix}
			<DiscoveryShelf
				title="Instant Mix"
				loading={mixQuery.isFetching}
				empty={!mixQuery.isFetching && mixTracks.length === 0}
				emptyMessage={mixQuery.isError ? "Couldn't build the mix." : 'Pick a genre to build a mix.'}
				onrefresh={mixGenre ? () => void mixQuery.refetch() : undefined}
			>
				{#snippet actions()}
					{#if genres.length > 0}
						<GenrePillFilter
							genres={genres.slice(0, 12)}
							selected={mixGenre || undefined}
							loading={mixQuery.isFetching}
							onselect={(g) => {
								if (g) mixGenre = g;
							}}
						/>
					{/if}
				{/snippet}
				{#if mixTracks.length > 0}
					<div class="flex items-center gap-2 mb-3 mt-3">
						<Button variant="primary" size="sm" onclick={() => play(mixTracks)}>Play all</Button>
						<Button variant="ghost" size="sm" onclick={() => play(mixTracks, 0, true)}>
							Shuffle
						</Button>
					</div>
					<DiscoveryTrackTable
						tracks={mixTracks.map(toDiscoveryTrack)}
						onplay={(i) => play(mixTracks, i)}
						{formatDuration}
					/>
				{/if}
			</DiscoveryShelf>
		{/if}

		{#if caps.random}
			<DiscoveryShelf
				title="Surprise Me"
				loading={randomQuery.isFetching}
				empty={!randomQuery.isFetching && randomTracks.length === 0}
				emptyMessage={randomQuery.isError
					? "Couldn't load random tracks."
					: 'Refresh to load a new batch of random tracks.'}
				onrefresh={() => void randomQuery.refetch()}
			>
				{#snippet actions()}
					{#if genres.length > 0}
						<GenrePillFilter
							genres={genres.slice(0, 12)}
							selected={randomGenre}
							loading={randomQuery.isFetching}
							showAll={true}
							onselect={(g) => (randomGenre = g)}
						/>
					{/if}
				{/snippet}
				{#if randomTracks.length > 0}
					<div class="flex items-center gap-2 mb-3 mt-3">
						<Button variant="primary" size="sm" onclick={() => play(randomTracks)}>Play all</Button>
						<Button variant="ghost" size="sm" onclick={() => play(randomTracks, 0, true)}>
							Shuffle
						</Button>
					</div>
					<DiscoveryTrackTable
						tracks={randomTracks.map(toDiscoveryTrack)}
						onplay={(i) => play(randomTracks, i)}
						{formatDuration}
					/>
				{/if}
			</DiscoveryShelf>
		{/if}

		{#if caps.recommendations}
			<DiscoveryShelf
				title="Recommended for you"
				loading={discoveryQuery.isFetching}
				empty={!discoveryQuery.isFetching && recommended.length === 0}
				emptyMessage={discoveryQuery.isError
					? "Couldn't load recommendations."
					: 'No recommendations available right now.'}
				onrefresh={() => void discoveryQuery.refetch()}
			>
				{#each recommended as group (group.title)}
					<div class="mb-4">
						<h3 class="text-sm font-medium text-base-content/70 mb-2">{group.title}</h3>
						<AlbumCarousel albums={group.albums} {onopen} />
					</div>
				{/each}
			</DiscoveryShelf>
		{/if}

		{#if genres.length > 0}
			<DiscoveryShelf title="Browse by Genre" emptyMessage="Pick a genre to browse tracks.">
				<GenreSongsBrowser
					{genres}
					fetchSongs={fetchGenreSongs}
					buildQueue={genreQueue}
					multiSelect={caps.multiGenre}
				/>
			</DiscoveryShelf>
		{/if}
	</DiscoveryZone>
{/if}
