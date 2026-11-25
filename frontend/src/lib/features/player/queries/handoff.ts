import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';

export type HandoffHeartbeat = components['schemas']['HandoffHeartbeat'];
export type HandoffRequest = components['schemas']['HandoffRequest'];
export type HandoffUpdate = components['schemas']['HandoffUpdate'];

export function syncHandoff(body: HandoffHeartbeat, signal: AbortSignal): Promise<HandoffUpdate> {
	return api.v3.POST(v3('/api/v3/playback/handoff/sync'), body, { signal });
}

export async function requestHandoff(body: HandoffRequest, signal: AbortSignal): Promise<void> {
	await api.v3.POST(v3('/api/v3/playback/handoff'), body, { signal });
}
