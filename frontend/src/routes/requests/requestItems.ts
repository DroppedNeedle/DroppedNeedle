import type { RequestItem, RequestKind } from '$lib/queries/requests/types';

export type RequestsTab =
	'active' | 'history' | 'wanted' | 'approvals' | 'auto-download' | 'upgrades';

export function itemKind(item: Pick<RequestItem, 'request_kind'>): RequestKind {
	return item.request_kind === 'track' ? 'track' : 'album';
}

export function requestKey(mbid: string, requestKind: RequestKind): string {
	return `${requestKind}:${mbid}`;
}

export function itemRequestKey(item: Pick<RequestItem, 'musicbrainz_id' | 'request_kind'>): string {
	return requestKey(item.musicbrainz_id, itemKind(item));
}

export function timeAgo(epochSeconds: number): string {
	const diff = Date.now() / 1000 - epochSeconds;
	if (diff < 60) return 'just now';
	if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
	if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
	return `${Math.floor(diff / 86400)}d ago`;
}
