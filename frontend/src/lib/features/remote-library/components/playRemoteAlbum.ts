import { launchRemotePlayback } from '$lib/features/player/playback/launchRemotePlayback';
import { compareDiscTrack } from '$lib/features/player/playback/queueHelpers';
import { readRemoteAlbumTracks } from '$lib/features/remote-library/queries/RemoteQueries.svelte';
import type { RemoteAlbum } from '$lib/features/remote-library/queries/types';
import { getCoverUrl } from '$lib/utils/errorHandling';

/**
 * Plays a server's album from its first track, the way the album dialog's
 * Play All does. False when it had no track to play.
 */
export async function playRemoteAlbum(album: RemoteAlbum): Promise<boolean> {
	const page = await readRemoteAlbumTracks(album.source, album.id);
	if (page.items.length === 0) return false;
	const albumId = album.release_group_mbid ?? album.id;
	launchRemotePlayback([...page.items].sort(compareDiscTrack), 0, false, {
		albumId,
		albumName: album.title,
		artistName: album.artist_name,
		coverUrl: getCoverUrl(album.image_url ?? null, albumId)
	});
	return true;
}
