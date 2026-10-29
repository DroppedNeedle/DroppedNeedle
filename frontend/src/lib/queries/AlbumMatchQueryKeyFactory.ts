import { key, userScope } from './keys';
import type { RemoteMatchHints } from './remotes/endpoints';
import type { RemoteSource } from './remotes/types';

type UserId = string | null | undefined;

// Keyed by user (the remote sources are the caller's own connections), then
// album, so one album's sources or all of a user's refresh together.
export const AlbumMatchKeyFactory = {
	user: (userId: UserId) => key(userScope(userId), 'album-match'),
	album: (userId: UserId, mbid: string) => [...AlbumMatchKeyFactory.user(userId), mbid] as const,
	local: (userId: UserId, mbid: string) =>
		[...AlbumMatchKeyFactory.album(userId, mbid), 'local'] as const,
	remote: (userId: UserId, source: RemoteSource, mbid: string, hints: RemoteMatchHints) =>
		[
			...AlbumMatchKeyFactory.album(userId, mbid),
			source,
			source === 'jellyfin' ? '' : (hints.title ?? ''),
			source === 'jellyfin' ? '' : (hints.artist ?? '')
		] as const
};
