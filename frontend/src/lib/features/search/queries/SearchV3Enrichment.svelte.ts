import type { components } from '$lib/api/v3/openapi';
import type { Album } from '$lib/features/album/queries/types';
import type { Artist } from '$lib/features/search/queries/types';
import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import type { EnrichmentBatchRequestV3, EnrichmentResponseV3 } from './SearchV3Queries.svelte';

type ArtistEnrichItemV3 = components['schemas']['ArtistEnrichmentRequest'];
type AlbumEnrichItemV3 = components['schemas']['AlbumEnrichmentRequest'];

// Hover/focus intent settles for this long before a batch goes out.
export const ENRICH_COLLECTOR_DELAY_MS = 120;
// The server honors this many ids per bucket; extras are ignored there.
export const MAX_ENRICH_IDS_PER_BUCKET = 10;

const emptyBody = (): EnrichmentBatchRequestV3 => ({ artists: [], albums: [] });

// Merge a delta enrichment response into previously collected enrichment rows.
export function mergeEnrichment(
	prev: EnrichmentResponseV3 | null,
	next: EnrichmentResponseV3
): EnrichmentResponseV3 {
	if (!prev) return next;
	const artists: Record<string, EnrichmentResponseV3['artists'][number]> = {};
	for (const item of prev.artists) {
		artists[item.musicbrainz_id] = item;
	}
	for (const item of next.artists) {
		artists[item.musicbrainz_id] = item;
	}
	const albums: Record<string, EnrichmentResponseV3['albums'][number]> = {};
	for (const item of prev.albums) {
		albums[item.musicbrainz_id] = item;
	}
	for (const item of next.albums) {
		albums[item.musicbrainz_id] = item;
	}
	return {
		artists: Object.values(artists),
		albums: Object.values(albums),
		source: next.source ?? prev.source,
		degradations: next.degradations
	};
}

// Collects card hover/focus intent into the reactive body behind
// getSearchEnrichBatchV3Query. Each flush commits only newly hovered ids so
// earlier cards are never re-sent on later hovers.
export class SearchEnrichCollector {
	// The latest delta batch to fetch.
	body = $state<EnrichmentBatchRequestV3>(emptyBody());

	#sentArtists = new SvelteSet<string>();
	#sentAlbums = new SvelteSet<string>();
	#pendingArtists = new SvelteMap<string, ArtistEnrichItemV3>();
	#pendingAlbums = new SvelteMap<string, AlbumEnrichItemV3>();
	#timer: ReturnType<typeof setTimeout> | null = null;
	#disposed = false;

	requestArtist(artist: Artist): void {
		if (this.#disposed || artist.listen_count != null) return;
		// Local-only rows carry no provider id, so enrichment has no key.
		if (artist.local_id === artist.musicbrainz_id) return;
		if (
			this.#sentArtists.has(artist.musicbrainz_id) ||
			this.#pendingArtists.has(artist.musicbrainz_id)
		) {
			return;
		}
		if (this.#sentArtists.size + this.#pendingArtists.size >= MAX_ENRICH_IDS_PER_BUCKET) return;
		this.#pendingArtists.set(artist.musicbrainz_id, {
			musicbrainz_id: artist.musicbrainz_id,
			name: artist.title
		});
		this.#schedule();
	}

	requestAlbum(album: Album): void {
		if (this.#disposed || album.listen_count != null) return;
		if (album.local_id === album.musicbrainz_id) return;
		if (
			this.#sentAlbums.has(album.musicbrainz_id) ||
			this.#pendingAlbums.has(album.musicbrainz_id)
		) {
			return;
		}
		if (this.#sentAlbums.size + this.#pendingAlbums.size >= MAX_ENRICH_IDS_PER_BUCKET) return;
		this.#pendingAlbums.set(album.musicbrainz_id, {
			musicbrainz_id: album.musicbrainz_id,
			artist_name: album.artist ?? '',
			album_name: album.title
		});
		this.#schedule();
	}

	reset(): void {
		if (this.#timer !== null) clearTimeout(this.#timer);
		this.#timer = null;
		this.#sentArtists.clear();
		this.#sentAlbums.clear();
		this.#pendingArtists.clear();
		this.#pendingAlbums.clear();
		this.body = emptyBody();
	}

	dispose(): void {
		this.#disposed = true;
		if (this.#timer !== null) clearTimeout(this.#timer);
		this.#timer = null;
		this.#sentArtists.clear();
		this.#sentAlbums.clear();
		this.#pendingArtists.clear();
		this.#pendingAlbums.clear();
	}

	#schedule(): void {
		if (this.#disposed || this.#timer !== null) return;
		this.#timer = setTimeout(() => this.#flush(), ENRICH_COLLECTOR_DELAY_MS);
	}

	#flush(): void {
		this.#timer = null;
		if (this.#disposed) return;
		if (this.#pendingArtists.size === 0 && this.#pendingAlbums.size === 0) return;
		const artists = [...this.#pendingArtists.values()];
		const albums = [...this.#pendingAlbums.values()];
		for (const item of artists) this.#sentArtists.add(item.musicbrainz_id);
		for (const item of albums) this.#sentAlbums.add(item.musicbrainz_id);
		this.body = { artists, albums };
		this.#pendingArtists.clear();
		this.#pendingAlbums.clear();
	}
}
