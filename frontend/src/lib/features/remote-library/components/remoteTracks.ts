import type { BrowseTrack } from '$lib/features/remote-library/components/GenreSongsBrowser.svelte';
import type { DiscoveryTrack } from '$lib/features/remote-library/components/DiscoveryTrackTable.svelte';
import type { RemoteTrack } from '$lib/features/remote-library/queries/types';

export function toDiscoveryTrack(track: RemoteTrack): DiscoveryTrack {
	return {
		id: track.id,
		title: track.title,
		artist_name: track.artist_name,
		album_name: track.album_name,
		album_id: track.album_id ?? undefined,
		image_url: track.image_url ?? undefined,
		duration_seconds: track.duration_secs ?? 0
	};
}

export function toBrowseTrack(track: RemoteTrack): BrowseTrack {
	return {
		id: track.id,
		title: track.title,
		artist_name: track.artist_name,
		album_name: track.album_name,
		duration_seconds: track.duration_secs ?? 0,
		image_url: track.image_url ?? undefined
	};
}
