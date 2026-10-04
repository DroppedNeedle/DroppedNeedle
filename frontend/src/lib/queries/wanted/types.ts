import type { components } from '$lib/api/v3/openapi';

// Transport types come from the generated v3 contract. The key is
// `musicbrainz_id`; states are `watching`/`paused` plus loop-set
// `dormant` and the legacy `stopped`/`fulfilled` the cards still render.
export type WantedWatchItem = components['schemas']['WantedItem'];
export type WantedRetryingItem = components['schemas']['WantedRetryingItem'];
export type WantedWatchesResponse = components['schemas']['WantedResponse'];
export type WantedActionResponse = components['schemas']['WantedActionResponse'];

export type WantedState = 'watching' | 'paused' | 'dormant' | 'stopped' | 'fulfilled';
