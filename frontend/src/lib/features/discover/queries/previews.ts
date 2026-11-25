import { v3 } from '$lib/api/v3/endpoint';
// The sampler's clip reads. It lives outside components, so it reaches the
// app's QueryClient through these.
import { api } from '$lib/api/client';
import { queryClient } from '$lib/queries/QueryClient';
import { PROVIDER } from '$lib/queries/persistedCache';
import { STALE_SLOW } from '$lib/queries/staleTime';
import { authStore } from '$lib/stores/authStore.svelte';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';
import type { AlbumPreviewMode } from './types';

/** A preview clip for one track. */
export function fetchTrackPreview(artist: string, track: string) {
	return queryClient.fetchQuery({
		meta: PROVIDER,
		queryKey: DiscoverQueryKeyFactory.v3.trackPreview(authStore.user?.id, artist, track),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/discover/track-preview', { query: { artist, track } }), { signal }),
		staleTime: STALE_SLOW
	});
}

/**
 * Preview clips for an album: its first tracks, or in `audition` its opener,
 * best-known track and a deep cut.
 */
export function fetchAlbumPreview(artist: string, album: string, mode: AlbumPreviewMode = 'first') {
	return queryClient.fetchQuery({
		meta: PROVIDER,
		queryKey: DiscoverQueryKeyFactory.v3.albumPreview(authStore.user?.id, artist, album, mode),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/discover/album-preview', { query: { artist, album, mode } }), {
				signal
			}),
		staleTime: STALE_SLOW
	});
}
