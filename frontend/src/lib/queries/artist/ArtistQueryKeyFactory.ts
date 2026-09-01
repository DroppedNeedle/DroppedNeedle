import type { MusicSource } from '$lib/stores/musicSource';
import { musicBrainzSourceKey } from '../musicbrainz/sourceScope.svelte';

const sourceScope = () => musicBrainzSourceKey();

export const ArtistQueryKeyFactory = {
	prefix: ['artist'] as const,
	basic: (id: string) => [...ArtistQueryKeyFactory.prefix, sourceScope(), id] as const,
	extended: (id: string) =>
		[...ArtistQueryKeyFactory.prefix, sourceScope(), id, 'extended'] as const,
	topAlbums: (id: string, source: MusicSource) =>
		[...ArtistQueryKeyFactory.prefix, sourceScope(), id, 'top-albums', { source }] as const,
	topSongs: (id: string, source: MusicSource) =>
		[...ArtistQueryKeyFactory.prefix, sourceScope(), id, 'top-songs', { source }] as const,
	lastFmEnrichment: (id: string, artistName?: string) =>
		[
			...ArtistQueryKeyFactory.prefix,
			sourceScope(),
			id,
			'lastfm-enrichment',
			{ artistName }
		] as const,
	releases: (id: string) =>
		[...ArtistQueryKeyFactory.prefix, sourceScope(), id, 'releases'] as const,
	similarArtists: (id: string, source: MusicSource) =>
		[...ArtistQueryKeyFactory.prefix, sourceScope(), id, 'similar-artists', { source }] as const
};
