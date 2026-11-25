import { v3 } from '$lib/api/v3/endpoint';
// The sampler's clip reads. It lives outside components, so it reaches the
// app's QueryClient through these.
import { api } from '$lib/api/client';
import { queryClient } from '$lib/queries/QueryClient';
import { PROVIDER } from '$lib/queries/persistedCache';
import { STALE_SLOW } from '$lib/queries/staleTime';
import { authStore } from '$lib/stores/authStore.svelte';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

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

/** Preview clips for an album's tracks. */
export function fetchAlbumPreview(artist: string, album: string) {
	return queryClient.fetchQuery({
		meta: PROVIDER,
		queryKey: DiscoverQueryKeyFactory.v3.albumPreview(authStore.user?.id, artist, album, null),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/discover/album-preview', { query: { artist, album } }), { signal }),
		staleTime: STALE_SLOW
	});
}
