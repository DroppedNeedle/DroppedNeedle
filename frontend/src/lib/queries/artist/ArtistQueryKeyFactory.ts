import type { MusicSource } from '$lib/stores/musicSource';
import { musicBrainzSourceKey } from '../musicbrainz/sourceScope.svelte';

const sourceScope = (userId: string | null | undefined) => musicBrainzSourceKey(userId);

export const ArtistQueryKeyFactory = {
	prefix: ['artist'] as const,
	basic: (id: string) => [...ArtistQueryKeyFactory.prefix, sourceScope(undefined), id] as const,
	extended: (id: string) =>
		[...ArtistQueryKeyFactory.prefix, sourceScope(undefined), id, 'extended'] as const,
	topAlbums: (userId: string | null | undefined, id: string, source: MusicSource) =>
		[
			...ArtistQueryKeyFactory.prefix,
			userId ?? null,
			sourceScope(userId),
			id,
			'top-albums',
			{ source }
		] as const,
	topSongs: (userId: string | null | undefined, id: string, source: MusicSource) =>
		[
			...ArtistQueryKeyFactory.prefix,
			userId ?? null,
			sourceScope(userId),
			id,
			'top-songs',
			{ source }
		] as const,
	lastFmEnrichment: (id: string, artistName?: string) =>
		[
			...ArtistQueryKeyFactory.prefix,
			sourceScope(undefined),
			id,
			'lastfm-enrichment',
			{ artistName }
		] as const,
	releases: (id: string) =>
		[...ArtistQueryKeyFactory.prefix, sourceScope(undefined), id, 'releases'] as const,
	similarArtists: (userId: string | null | undefined, id: string, source: MusicSource) =>
		[
			...ArtistQueryKeyFactory.prefix,
			userId ?? null,
			sourceScope(userId),
			id,
			'similar-artists',
			{ source }
		] as const
};
