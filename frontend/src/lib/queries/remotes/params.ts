// Every remote route carries the `{source}` segment (jellyfin/navidrome/plex):
// one adapter behind one path shape, so per-source UI branches collapse into a
// source parameter.
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
	/** Plex only. */
	mood?: string;
	/** Jellyfin only. */
	tags?: string;
	/** Jellyfin only. */
	studios?: string;
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

/** The album's title and artist, so Plex and Navidrome can match by name. */
export interface RemoteMatchHints {
	title?: string;
	artist?: string;
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

// Unset and empty values stay out of the query string (every adapter param
// is a string or number).
export function remoteQuery<P extends object>(params: P): P {
	const out: Record<string, string | number> = {};
	for (const [key, value] of Object.entries(params)) {
		if (typeof value !== 'string' && typeof value !== 'number') continue;
		if (value !== '') out[key] = value;
	}
	return out as P;
}
