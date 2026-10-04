import type { AlbumIdentityState, LibraryAlbumSummary } from '$lib/types';
import type { AlbumViewV3 } from './LibraryV3Queries.svelte';

// Adapters from v3 catalog views to the browse shapes shared cards render.
// LibraryAlbumCard is shared across surfaces, so the albums page adapts at
// the query edge instead of retyping the card.

function toIdentityState(view: AlbumViewV3): AlbumIdentityState {
	if (view.identity_state !== 'linked') return 'local_only';
	if (view.release_mbid) return 'release_linked';
	return 'release_group_linked';
}

export function albumViewToSummary(view: AlbumViewV3): LibraryAlbumSummary {
	return {
		id: view.id,
		title: view.title,
		artist_name: view.artist_name,
		artist_id: view.artist_id,
		musicbrainz_release_group_id: view.release_group_mbid ?? null,
		musicbrainz_release_id: view.release_mbid ?? null,
		musicbrainz_artist_id: view.artist_mbid ?? null,
		album_identity_state: toIdentityState(view),
		track_count: view.track_count,
		total_duration_seconds: view.total_duration_seconds,
		total_size_bytes: view.total_size_bytes,
		format: view.format ?? null,
		year: view.year ?? null,
		is_compilation: view.is_compilation,
		release_type: null,
		cover_available: view.cover_available,
		date_added: view.date_added ?? null,
		sort_name: null,
		original_release_date: null,
		contribution_id: null,
		contribution_state: null
	};
}
