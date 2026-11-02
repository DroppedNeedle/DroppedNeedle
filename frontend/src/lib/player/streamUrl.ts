import { getApiUrl } from '$lib/api/api-utils';
import { gatewayStreamUrl, type StreamHints } from './playbackGateway';
import type { QueueItem, SourceType } from './types';

type StreamTarget = Pick<QueueItem, 'sourceType' | 'trackSourceId'>;
type NativeTarget = { sourceType: Exclude<SourceType, 'youtube'>; trackSourceId: string };

// The gateway path for a track, before the base path. YouTube plays through
// its embed and has none.
function streamPath(item: StreamTarget, hints?: StreamHints): string | null {
	if (item.sourceType === 'youtube') return null;
	return gatewayStreamUrl(item.sourceType, item.trackSourceId, hints);
}

/** The address an audio element loads for this track, with the base path applied. */
export function streamUrlFor(item: NativeTarget, hints?: StreamHints): string;
export function streamUrlFor(item: StreamTarget, hints?: StreamHints): string | undefined;
export function streamUrlFor(item: StreamTarget, hints?: StreamHints): string | undefined {
	const path = streamPath(item, hints);
	return path === null ? undefined : getApiUrl(path);
}

/** The path to warm ahead of a track; the API client applies the base path. */
export function prefetchPathFor(item: StreamTarget): string | null {
	return streamPath(item);
}
