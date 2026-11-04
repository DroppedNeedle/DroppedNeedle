import type { components } from '$lib/api/v3/openapi';

type Schemas = components['schemas'];

export type DownloadTask = Schemas['DownloadTaskResponse'];

export type DownloadActivitySummary = Schemas['DownloadActivitySummaryResponse'];

/** One download or upload whose files wait for a decision. */
export type HeldEntry = Schemas['HeldEntryResponse'];

export type HeldFile = Schemas['HeldFileResponse'];
