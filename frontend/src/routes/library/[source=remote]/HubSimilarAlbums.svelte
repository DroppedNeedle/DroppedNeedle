<script lang="ts">
	import HubShelf from '$lib/components/HubShelf.svelte';
	import { reveal } from '$lib/actions/reveal';
	import { getRemoteSimilarQuery } from '$lib/queries/remotes/RemoteQueries.svelte';
	import type { RemoteSourceDescriptor } from '$lib/queries/remotes/remoteSources';
	import type { RemoteAlbum, RemoteTrack } from '$lib/queries/remotes/types';
	import AlbumCarousel from './AlbumCarousel.svelte';

	interface Props {
		remote: RemoteSourceDescriptor;
		/** The album the shelf is like. */
		seed: RemoteAlbum | null;
		onopen: (album: RemoteAlbum) => void;
	}

	let { remote, seed, onopen }: Props = $props();

	const similarQuery = getRemoteSimilarQuery(
		() => remote.source,
		() => seed?.id ?? '',
		() => ({ limit: 30 })
	);
	const seedName = $derived(seed?.title ?? '');
	const loading = $derived(similarQuery.isFetching);
	// The similar route answers tracks; the shelf shows their albums.
	const albums = $derived(groupByAlbum(similarQuery.data?.items ?? []));

	function groupByAlbum(tracks: RemoteTrack[]): RemoteAlbum[] {
		const albums: RemoteAlbum[] = [];
		const seen: string[] = [];
		for (const t of tracks) {
			const key = t.album_id ?? t.album_name;
			if (!key || seen.includes(key)) continue;
			seen.push(key);
			albums.push({
				source: t.source,
				id: t.album_id ?? key,
				title: t.album_name,
				artist_name: t.artist_name,
				image_url: t.image_url ?? null,
				year: t.year ?? null
			});
		}
		return albums;
	}
</script>

{#if seedName || loading || albums.length > 0}
	<div use:reveal>
		<HubShelf title={seedName ? `More Like "${seedName}"` : 'More Like This'} {loading}>
			{#if albums.length > 0}
				<AlbumCarousel {albums} {onopen} />
			{:else if similarQuery.isError}
				<p class="text-sm text-base-content/50">Couldn't load similar albums.</p>
			{:else if !loading}
				<p class="text-sm text-base-content/50">No similar albums found.</p>
			{/if}
		</HubShelf>
	</div>
{/if}
