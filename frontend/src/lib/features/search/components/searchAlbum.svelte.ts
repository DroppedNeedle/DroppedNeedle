import type { Album } from '$lib/features/album/queries/types';
import { createRequestAlbumMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
import { libraryStore } from '$lib/stores/library';

/** A search album row that exists only in the library (no MusicBrainz id). */
export function isLocalOnlyAlbum(album: Album): boolean {
	return Boolean(album.local_id && album.local_id === album.musicbrainz_id);
}

/** `removed`: the row's own flags are stale since an admin removed it here. */
export function albumInLibrary(album: Album, removed = false): boolean {
	return libraryStore.isInLibrary(album.musicbrainz_id) || (!removed && album.in_library);
}

export function albumRequested(album: Album, removed = false): boolean {
	return (
		!albumInLibrary(album, removed) &&
		(libraryStore.isRequested(album.musicbrainz_id) || (!removed && Boolean(album.requested)))
	);
}

/** Requests a search album, with the pending state its button shows. */
export function createSearchAlbumRequest() {
	const mutation = createRequestAlbumMutation();
	let pending = $state(false);

	return {
		get pending() {
			return pending;
		},
		async request(album: Album): Promise<void> {
			pending = true;
			try {
				await mutation
					.mutateAsync({
						release_group_mbid: album.musicbrainz_id,
						artist_name: album.artist ?? undefined,
						album_title: album.title,
						year: album.year ?? undefined
					})
					.catch(() => null);
			} finally {
				pending = false;
			}
		}
	};
}
