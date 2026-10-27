import type { components } from '$lib/api/v3/openapi';

/** Album vs track request lane, carried on request query strings. */
export type RequestKind = components['schemas']['RequestKind'];
export type RequestItem = components['schemas']['RequestItem'];
export type IntakeResponse = components['schemas']['IntakeResponse'];
export type TrackIntakeResponse = components['schemas']['TrackIntakeResponse'];
