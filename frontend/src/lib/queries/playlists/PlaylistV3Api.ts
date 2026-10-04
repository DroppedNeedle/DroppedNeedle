import { v3 } from '$lib/api/v3/endpoint';
import type { FavoriteV3Kind } from './PlaylistQueryKeyFactory';

// /api/v3 playlist and favorites URLs, built through the typed registry:
// every template is a literal the contract-coverage gate verifies against
// the generated spec. Nothing outside this feature imports them.
export const PlaylistV3Api = {
	list: () => v3('/api/v3/playlists'),
	create: () => v3('/api/v3/playlists'),
	detail: (id: string) => v3('/api/v3/playlists/{playlist_id}', { path: { playlist_id: id } }),
	tracks: (id: string) =>
		v3('/api/v3/playlists/{playlist_id}/tracks', { path: { playlist_id: id } }),
	removeTracks: (id: string) =>
		v3('/api/v3/playlists/{playlist_id}/tracks/remove', { path: { playlist_id: id } }),
	reorderTrack: (id: string) =>
		v3('/api/v3/playlists/{playlist_id}/tracks/reorder', { path: { playlist_id: id } }),
	track: (id: string, trackId: string) =>
		v3('/api/v3/playlists/{playlist_id}/tracks/{track_id}', {
			path: { playlist_id: id, track_id: trackId }
		}),
	visibility: (id: string) =>
		v3('/api/v3/playlists/{playlist_id}/visibility', { path: { playlist_id: id } }),
	cover: (id: string) => v3('/api/v3/playlists/{playlist_id}/cover', { path: { playlist_id: id } }),
	checkTracks: () => v3('/api/v3/playlists/check-tracks'),
	resolveSources: (id: string) =>
		v3('/api/v3/playlists/{playlist_id}/resolve-sources', { path: { playlist_id: id } }),
	favorites: (kind: FavoriteV3Kind | null) =>
		kind ? v3('/api/v3/favorites', { query: { kind } }) : v3('/api/v3/favorites'),
	favorite: (kind: FavoriteV3Kind, itemId: string) =>
		v3('/api/v3/favorites/{kind}/{item_id}', { path: { kind, item_id: itemId } })
};
