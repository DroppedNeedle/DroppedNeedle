import { v3, type V3Query } from '$lib/api/v3/endpoint';
import type { RemoteSource } from './types';

// v3 remote-adapter URLs, built through the typed registry: every template
// is a literal the contract-coverage gate verifies against the generated
// spec, and hooks import from here so a route rename touches this file
// only. Every route carries the `{source}` segment
// (jellyfin/navidrome/plex): one adapter behind one path shape, so
// per-source UI branches collapse into a source parameter.
export interface RemotePageParams {
	limit?: number;
	offset?: number;
}

export interface RemoteAlbumParams extends RemotePageParams {
	sort_by?: string;
	sort_order?: string;
	genre?: string;
	year?: number;
	decade?: string;
}

export interface RemoteArtistParams extends RemotePageParams {
	sort_by?: string;
	sort_order?: string;
	search?: string;
}

export interface RemoteTrackParams extends RemotePageParams {
	sort_by?: string;
	sort_order?: string;
	search?: string;
	genre?: string;
}

export interface RemoteSearchParams {
	q: string;
	limit?: number;
}

export interface RemoteLyricsParams {
	artist?: string;
	title?: string;
}

export interface RemoteMixParams {
	kind?: string;
	limit?: number;
}

export interface RemoteRandomParams {
	limit?: number;
	genre?: string;
}

// The param interfaces carry no index signature, so the builder takes a
// plain object and narrows each entry (every adapter param is a string or
// number today). Unset and empty values stay out of the query string.
function query(params: object): V3Query {
	const out: V3Query = {};
	for (const [key, value] of Object.entries(params)) {
		if (typeof value !== 'string' && typeof value !== 'number') continue;
		if (value !== '') out[key] = value;
	}
	return out;
}

export const REMOTE_ENDPOINTS = {
	hub: (source: RemoteSource) => v3('/api/v3/remotes/{source}/hub', { path: { source } }),
	connection: (source: RemoteSource) =>
		v3('/api/v3/remotes/{source}/connection', { path: { source } }),
	albums: (source: RemoteSource, params: RemoteAlbumParams = {}) =>
		v3('/api/v3/remotes/{source}/albums', { path: { source }, query: query(params) }),
	album: (source: RemoteSource, id: string) =>
		v3('/api/v3/remotes/{source}/albums/{id}', { path: { source, id } }),
	albumTracks: (source: RemoteSource, id: string, params: RemotePageParams = {}) =>
		v3('/api/v3/remotes/{source}/albums/{id}/tracks', {
			path: { source, id },
			query: query(params)
		}),
	artists: (source: RemoteSource, params: RemoteArtistParams = {}) =>
		v3('/api/v3/remotes/{source}/artists', { path: { source }, query: query(params) }),
	artistIndex: (source: RemoteSource) =>
		v3('/api/v3/remotes/{source}/artists/index', { path: { source } }),
	artist: (source: RemoteSource, id: string) =>
		v3('/api/v3/remotes/{source}/artists/{id}', { path: { source, id } }),
	tracks: (source: RemoteSource, params: RemoteTrackParams = {}) =>
		v3('/api/v3/remotes/{source}/tracks', { path: { source }, query: query(params) }),
	search: (source: RemoteSource, params: RemoteSearchParams) =>
		v3('/api/v3/remotes/{source}/search', { path: { source }, query: query(params) }),
	favorites: (source: RemoteSource) =>
		v3('/api/v3/remotes/{source}/favorites', { path: { source } }),
	recent: (source: RemoteSource, params: RemotePageParams = {}) =>
		v3('/api/v3/remotes/{source}/recent', { path: { source }, query: query(params) }),
	recentlyAdded: (source: RemoteSource, params: RemotePageParams = {}) =>
		v3('/api/v3/remotes/{source}/recently-added', { path: { source }, query: query(params) }),
	history: (source: RemoteSource, params: RemotePageParams = {}) =>
		v3('/api/v3/remotes/{source}/history', { path: { source }, query: query(params) }),
	stats: (source: RemoteSource) => v3('/api/v3/remotes/{source}/stats', { path: { source } }),
	sessions: (source: RemoteSource) => v3('/api/v3/remotes/{source}/sessions', { path: { source } }),
	genres: (source: RemoteSource) => v3('/api/v3/remotes/{source}/genres', { path: { source } }),
	genreSongs: (source: RemoteSource, genre: string, params: RemotePageParams = {}) =>
		v3('/api/v3/remotes/{source}/genres/songs', {
			path: { source },
			query: query({ genre, ...params })
		}),
	playlists: (source: RemoteSource, params: RemotePageParams = {}) =>
		v3('/api/v3/remotes/{source}/playlists', { path: { source }, query: query(params) }),
	playlist: (source: RemoteSource, id: string) =>
		v3('/api/v3/remotes/{source}/playlists/{id}', { path: { source, id } }),
	importPlaylist: (source: RemoteSource, id: string) =>
		v3('/api/v3/remotes/{source}/playlists/{id}/import', { path: { source, id } }),
	infoAlbum: (source: RemoteSource, id: string) =>
		v3('/api/v3/remotes/{source}/info/albums/{id}', { path: { source, id } }),
	infoArtist: (source: RemoteSource, id: string) =>
		v3('/api/v3/remotes/{source}/info/artists/{id}', { path: { source, id } }),
	lyrics: (source: RemoteSource, id: string, params: RemoteLyricsParams = {}) =>
		v3('/api/v3/remotes/{source}/lyrics/{id}', {
			path: { source, id },
			query: query(params)
		}),
	image: (source: RemoteSource, id: string, size?: number) =>
		v3('/api/v3/remotes/{source}/images/{id}', {
			path: { source, id },
			query: query({ size })
		}),
	playlistCover: (source: RemoteSource, id: string, size?: number) =>
		v3('/api/v3/remotes/{source}/covers/playlists/{id}', {
			path: { source, id },
			query: query({ size })
		}),
	match: (source: RemoteSource, mbid: string) =>
		v3('/api/v3/remotes/{source}/match', { path: { source }, query: query({ mbid }) }),
	mix: (source: RemoteSource, id: string, params: RemoteMixParams = {}) =>
		v3('/api/v3/remotes/{source}/mix/{id}', {
			path: { source, id },
			query: query(params)
		}),
	similar: (source: RemoteSource, id: string, params: RemotePageParams = {}) =>
		v3('/api/v3/remotes/{source}/similar/{id}', {
			path: { source, id },
			query: query(params)
		}),
	top: (source: RemoteSource, artist: string, limit?: number) =>
		v3('/api/v3/remotes/{source}/top/{artist}', {
			path: { source, artist },
			query: query({ limit })
		}),
	random: (source: RemoteSource, params: RemoteRandomParams = {}) =>
		v3('/api/v3/remotes/{source}/random', { path: { source }, query: query(params) }),
	discovery: (source: RemoteSource, count?: number) =>
		v3('/api/v3/remotes/{source}/discovery', {
			path: { source },
			query: query({ count })
		}),
	folders: () => v3('/api/v3/remotes/navidrome/folders')
} as const;
