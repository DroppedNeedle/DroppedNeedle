import type { Artist, Album, EnrichmentSource } from '$lib/types';
import type { EnrichmentResponseV3 } from '../queries/search/SearchV3Queries.svelte';

export function getListenTitle(
	source: EnrichmentSource,
	kind: 'artist' | 'album' = 'artist'
): string {
	if (source === 'lastfm') return kind === 'album' ? 'Last.fm plays' : 'Last.fm listeners';
	if (source === 'listenbrainz') return 'ListenBrainz plays';
	return 'Plays';
}

// Degradations ride on the v3 answer and are read by the query stale-time
// helper; counts simply stay absent for degraded ids, so these merges
// ignore the degradations list and map what arrived.
export function applyArtistEnrichment(
	artists: Artist[],
	enrichment: EnrichmentResponseV3
): Artist[] {
	if (enrichment.artists.length === 0) return artists;

	const map = new Map(enrichment.artists.map((a) => [a.musicbrainz_id, a]));
	return artists.map((artist) => {
		const enrich = map.get(artist.musicbrainz_id);
		if (!enrich) return artist;
		return {
			...artist,
			release_group_count: enrich.release_group_count ?? artist.release_group_count,
			listen_count: enrich.listen_count ?? artist.listen_count
		};
	});
}

export function applyAlbumEnrichment(albums: Album[], enrichment: EnrichmentResponseV3): Album[] {
	if (enrichment.albums.length === 0) return albums;

	const map = new Map(enrichment.albums.map((a) => [a.musicbrainz_id, a]));
	return albums.map((album) => {
		const enrich = map.get(album.musicbrainz_id);
		if (!enrich) return album;
		return {
			...album,
			track_count: enrich.track_count ?? album.track_count,
			listen_count: enrich.listen_count ?? album.listen_count
		};
	});
}
