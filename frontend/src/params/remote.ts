import type { ParamMatcher } from '@sveltejs/kit';
import { isRemoteSource } from '$lib/features/remote-library/queries/remoteSources';
import type { RemoteSource } from '$lib/features/remote-library/queries/types';

/** The media servers with a library tree under /library: plex, jellyfin, navidrome. */
export const match = ((param: string): param is RemoteSource =>
	isRemoteSource(param)) satisfies ParamMatcher;
