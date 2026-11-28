import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { libraryTrackBlockQueryOptions } from '$lib/features/library/queries/LibraryQueries.svelte';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

export type SimilarTrack = components['schemas']['SimilarTrackView'];
export type Closeness = components['schemas']['Closeness'];
export type BenchTrack = components['schemas']['BenchTrack'];

/** How many stations the bench sets out. */
export const BENCH_STATIONS = 10;
/** The furthest a station can be, and the tuner's right end (the server's cut-off). */
export const BENCH_CUT_OFF = 0.15;

/** Whether `error` says sound analysis is off or its settings cannot be read. */
export function soundOff(error: unknown): boolean {
	return (
		error instanceof ApiError &&
		(error.code === 'SOUND_ANALYSIS_OFF' || error.code === 'SOUND_ANALYSIS_UNREADABLE')
	);
}

// A 409 or a 404 answers the same until something changes: no retry.
const retry = (count: number, error: unknown) =>
	!(error instanceof ApiError && error.status < 500) && count < 2;

/** The library tracks that sound most like one track, nearest first. */
export const getSimilarTracksQuery = (getTrackId: Getter<string | null>) =>
	createQuery(() => {
		const trackId = getTrackId() ?? '';
		return {
			enabled: Boolean(trackId && authStore.user?.id),
			queryKey: DiscoverQueryKeyFactory.v3.soundSimilar(authStore.user?.id, trackId),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.v3.GET(
					v3('/api/v3/library/tracks/{id}/similar', {
						path: { id: trackId },
						query: { limit: BENCH_STATIONS }
					}),
					{ signal }
				),
			retry
		};
	});

/** One track's 200 stored sound measures at the analysis version serving now. */
export const getTrackSoundQuery = (getTrackId: Getter<string | null>, getVersion: Getter<number>) =>
	createQuery(() => {
		const trackId = getTrackId() ?? '';
		return {
			enabled: Boolean(trackId && authStore.user?.id),
			queryKey: DiscoverQueryKeyFactory.v3.soundMeasures(authStore.user?.id, trackId, getVersion()),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.v3.GET(v3('/api/v3/library/tracks/{id}/sound', { path: { id: trackId } }), {
					signal
				}),
			staleTime: Infinity,
			retry
		};
	});

/** Library tracks matching `getText()`, for picking the bench's track; nothing under two letters. */
export const getBenchTrackSearchQuery = (getText: Getter<string>) =>
	createQuery(() => {
		const q = getText().trim();
		return {
			...libraryTrackBlockQueryOptions(authStore.user?.id, { limit: 12, q }, 0),
			enabled: q.length >= 2 && Boolean(authStore.user?.id)
		};
	});
