import type { components } from '$lib/api/v3/openapi';

type Schemas = components['schemas'];

export type TopSong = Schemas['TopSong'];

/** A popular song's position, answered with the library file that holds it. */
export type ResolvedTrack = Schemas['ResolvedTrack'];
