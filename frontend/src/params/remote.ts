import type { ParamMatcher } from '@sveltejs/kit';
import { isRemoteSource } from '$lib/queries/remotes/remoteSources';
import type { RemoteSource } from '$lib/queries/remotes/types';

/** The media servers with a library tree under /library: plex, jellyfin, navidrome. */
export const match = ((param: string): param is RemoteSource =>
	isRemoteSource(param)) satisfies ParamMatcher;
