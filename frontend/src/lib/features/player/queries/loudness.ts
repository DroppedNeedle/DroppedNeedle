import type { components } from '$lib/api/v3/openapi';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { key, SHARED } from '$lib/queries/keys';
import { queryOptions } from '@tanstack/svelte-query';

export type TrackLoudness = components['schemas']['TrackLoudnessView'];

// A library track's levelling numbers are the same for everyone.
export const PlayerLoudnessQueryKeyFactory = {
	feature: 'player',
	track: (trackId: string) => key(SHARED, 'player', 'loudness', trackId)
};

/** A library track's gains and peaks for evening out volume: measured first, tags second. */
export const getTrackLoudnessQueryOptions = (trackId: string) =>
	queryOptions({
		queryKey: PlayerLoudnessQueryKeyFactory.track(trackId),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/library/tracks/{id}/loudness', { path: { id: trackId } }), {
				signal
			})
	});
