<script lang="ts">
	import DiscoveryShelf from '$lib/features/remote-library/components/DiscoveryShelf.svelte';
	import DiscoveryTrackTable from '$lib/features/remote-library/components/DiscoveryTrackTable.svelte';
	import DiscoveryZone from '$lib/features/remote-library/components/DiscoveryZone.svelte';
	import GenrePillFilter from '$lib/features/remote-library/components/GenrePillFilter.svelte';
	import GenreSongsBrowser, {
		type BrowseTrack
	} from '$lib/features/remote-library/components/GenreSongsBrowser.svelte';
	import { Box, Radio, Sparkles } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import {
		fetchRemoteGenreSongs,
		getRemoteDiscoveryQuery,
		getRemoteMixQuery,
		getRemoteRandomQuery
	} from '$lib/features/remote-library/queries/RemoteQueries.svelte';
	import type { RemoteSourceDescriptor } from '$lib/features/remote-library/queries/remoteSources';
	import type { RemoteAlbum, RemoteTrack } from '$lib/features/remote-library/queries/types';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { formatDurationSec as formatDuration } from '$lib/utils/formatting';
	import { SvelteMap } from 'svelte/reactivity';
	import AlbumCarousel from './AlbumCarousel.svelte';
	import { toBrowseTrack, toDiscoveryTrack } from './remoteTracks';

	interface Props {
		remote: RemoteSourceDescriptor;
		genres: string[];
		onopen: (album: RemoteAlbum) => void;
		/** A shelf drawn beside the first discovery shelf on a wide screen (Favorites). */
		lead?: Snippet;
	}

	let { remote, genres, onopen, lead }: Props = $props();
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
	const showMix = $derived(caps.mix && (genres.length > 0 || mixTracks.length > 0));

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

{#snippet mixShelf()}
	<DiscoveryShelf
		title="Instant Mix"
		icon={Radio}
		loading={mixQuery.isFetching}
		empty={!mixQuery.isFetching && mixTracks.length === 0}
		emptyMessage={mixQuery.isError ? "Couldn't build the mix." : 'Pick a genre to build a mix.'}
		onrefresh={mixGenre ? () => void mixQuery.refetch() : undefined}
		onplayall={() => play(mixTracks)}
		onshuffle={() => play(mixTracks, 0, true)}
	>
		{#snippet filters()}
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
		<DiscoveryTrackTable
			tracks={mixTracks.map(toDiscoveryTrack)}
			onplay={(i) => play(mixTracks, i)}
			{formatDuration}
		/>
	</DiscoveryShelf>
{/snippet}

{#snippet randomShelf()}
	<DiscoveryShelf
		title="Surprise Me"
		icon={Box}
		sub="20 random tracks"
		loading={randomQuery.isFetching}
		empty={!randomQuery.isFetching && randomTracks.length === 0}
		emptyMessage={randomQuery.isError
			? "Couldn't load random tracks."
			: 'Refresh to load a new batch of random tracks.'}
		onrefresh={() => void randomQuery.refetch()}
		onplayall={() => play(randomTracks)}
		onshuffle={() => play(randomTracks, 0, true)}
	>
		{#snippet filters()}
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
		<DiscoveryTrackTable
			tracks={randomTracks.map(toDiscoveryTrack)}
			onplay={(i) => play(randomTracks, i)}
			{formatDuration}
		/>
	</DiscoveryShelf>
{/snippet}

<DiscoveryZone>
	{#if lead || showMix || caps.random}
		<!-- Favorites beside the first discovery shelf; either runs full width alone -->
		<div class="hub-pair">
			{@render lead?.()}
			{#if showMix}
				{@render mixShelf()}
			{:else if caps.random}
				{@render randomShelf()}
			{/if}
		</div>
	{/if}

	{#if showMix && caps.random}
		{@render randomShelf()}
	{/if}

	{#if caps.recommendations}
		<DiscoveryShelf
			title="Recommended for you"
			icon={Sparkles}
			bare
			loading={discoveryQuery.isFetching}
			empty={!discoveryQuery.isFetching && recommended.length === 0}
			emptyMessage={discoveryQuery.isError
				? "Couldn't load recommendations."
				: 'No recommendations available right now.'}
			onrefresh={() => void discoveryQuery.refetch()}
		>
			<div class="space-y-6">
				{#each recommended as group (group.title)}
					<div>
						<h3 class="hub-sub mb-2">{group.title}</h3>
						<AlbumCarousel albums={group.albums} {onopen} />
					</div>
				{/each}
			</div>
		</DiscoveryShelf>
	{/if}

	{#if genres.length > 0}
		<GenreSongsBrowser
			{genres}
			fetchSongs={fetchGenreSongs}
			buildQueue={genreQueue}
			multiSelect={caps.multiGenre}
		/>
	{/if}
</DiscoveryZone>
