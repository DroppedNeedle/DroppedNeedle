import type { components } from '$lib/api/v3/openapi';
import type { Album, Artist } from '$lib/types';
import { SvelteMap } from 'svelte/reactivity';
import type { EnrichmentBatchRequestV3 } from './SearchV3Queries.svelte';

type ArtistEnrichItemV3 = components['schemas']['ArtistEnrichmentRequest'];
type AlbumEnrichItemV3 = components['schemas']['AlbumEnrichmentRequest'];

// Hover/focus intent settles for this long before a batch goes out.
export const ENRICH_COLLECTOR_DELAY_MS = 120;
// The server honors this many ids per bucket; extras are ignored there.
export const MAX_ENRICH_IDS_PER_BUCKET = 10;

const emptyBody = (): EnrichmentBatchRequestV3 => ({ artists: [], albums: [] });

// Collects card hover/focus intent into the reactive body behind
// getSearchEnrichBatchV3Query. Requests debounce into one fingerprint per
// burst, so a sweep across cards sends a single POST for the accumulated
// ids instead of one per hover.
export class SearchEnrichCollector {
	// The committed body. Every commit is cumulative: the query refires per
	// fingerprint change and each answer covers every id collected so far.
	body = $state<EnrichmentBatchRequestV3>(emptyBody());

	#pendingArtists = new SvelteMap<string, ArtistEnrichItemV3>();
	#pendingAlbums = new SvelteMap<string, AlbumEnrichItemV3>();
	#timer: ReturnType<typeof setTimeout> | null = null;
	#disposed = false;

	requestArtist(artist: Artist): void {
		if (this.#disposed || artist.listen_count != null) return;
		// Local-only rows carry no provider id, so enrichment has no key.
		if (artist.local_id === artist.musicbrainz_id) return;
		if (this.#artistIds().includes(artist.musicbrainz_id)) return;
		if (this.#artistIds().length + this.#pendingArtists.size >= MAX_ENRICH_IDS_PER_BUCKET) return;
		this.#pendingArtists.set(artist.musicbrainz_id, {
			musicbrainz_id: artist.musicbrainz_id,
			name: artist.title
		});
		this.#schedule();
	}

	requestAlbum(album: Album): void {
		if (this.#disposed || album.listen_count != null) return;
		if (album.local_id === album.musicbrainz_id) return;
		if (this.#albumIds().includes(album.musicbrainz_id)) return;
		if (this.#albumIds().length + this.#pendingAlbums.size >= MAX_ENRICH_IDS_PER_BUCKET) return;
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
		this.#pendingArtists.clear();
		this.#pendingAlbums.clear();
		this.body = emptyBody();
	}

	dispose(): void {
		this.#disposed = true;
		if (this.#timer !== null) clearTimeout(this.#timer);
		this.#timer = null;
		this.#pendingArtists.clear();
		this.#pendingAlbums.clear();
	}

	#artistIds(): string[] {
		return (this.body.artists ?? []).map((item) => item.musicbrainz_id);
	}

	#albumIds(): string[] {
		return (this.body.albums ?? []).map((item) => item.musicbrainz_id);
	}

	#schedule(): void {
		if (this.#disposed || this.#timer !== null) return;
		this.#timer = setTimeout(() => this.#flush(), ENRICH_COLLECTOR_DELAY_MS);
	}

	#flush(): void {
		this.#timer = null;
		if (this.#disposed) return;
		if (this.#pendingArtists.size === 0 && this.#pendingAlbums.size === 0) return;
		this.body = {
			artists: [...(this.body.artists ?? []), ...this.#pendingArtists.values()].slice(
				0,
				MAX_ENRICH_IDS_PER_BUCKET
			),
			albums: [...(this.body.albums ?? []), ...this.#pendingAlbums.values()].slice(
				0,
				MAX_ENRICH_IDS_PER_BUCKET
			)
		};
		this.#pendingArtists.clear();
		this.#pendingAlbums.clear();
	}
}
