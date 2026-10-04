import type { RequestKind } from '$lib/constants';
import { v3, type V3Query } from '$lib/api/v3/endpoint';

// v3 requests URLs, built through the typed registry: every template is a
// literal the contract-coverage gate verifies against the generated spec,
// and hooks import from here so a route rename touches this file only. v3
// names the kind filter `kind` (v2 `request_kind`).
export interface HistoryParams {
	page?: number;
	pageSize?: number;
	status?: string;
	sort?: string;
	kind?: RequestKind;
}

function historyQuery(params: HistoryParams): V3Query {
	const query: V3Query = {};
	if (params.page !== undefined) query.page = params.page;
	if (params.pageSize !== undefined) query.page_size = params.pageSize;
	if (params.status) query.status = params.status;
	if (params.sort) query.sort = params.sort;
	if (params.kind) query.kind = params.kind;
	return query;
}

export const REQUESTS_ENDPOINTS = {
	active: () => v3('/api/v3/requests/active'),
	activeCount: () => v3('/api/v3/requests/active/count'),
	history: (params: HistoryParams = {}) =>
		v3('/api/v3/requests/history', { query: historyQuery(params) }),
	cancel: (musicbrainzId: string, kind: RequestKind = 'album') =>
		v3('/api/v3/requests/active/{musicbrainz_id}', {
			path: { musicbrainz_id: musicbrainzId },
			query: { kind }
		}),
	retry: (musicbrainzId: string, kind: RequestKind = 'album') =>
		v3('/api/v3/requests/retry/{musicbrainz_id}', {
			path: { musicbrainz_id: musicbrainzId },
			query: { kind }
		}),
	clearHistory: (musicbrainzId: string, kind: RequestKind = 'album') =>
		v3('/api/v3/requests/history/{musicbrainz_id}', {
			path: { musicbrainz_id: musicbrainzId },
			query: { kind }
		}),
	requestAlbum: () => v3('/api/v3/requests/albums'),
	requestTrack: () => v3('/api/v3/requests/tracks'),
	requestBatch: () => v3('/api/v3/requests/batches'),
	batchCancel: () => v3('/api/v3/requests/batches/cancel'),
	sync: () => v3('/api/v3/requests/sync'),
	approvals: () => v3('/api/v3/requests/approvals'),
	approvalsCount: () => v3('/api/v3/requests/approvals/count'),
	approve: (musicbrainzId: string, kind: RequestKind = 'album') =>
		v3('/api/v3/requests/approvals/{musicbrainz_id}/approve', {
			path: { musicbrainz_id: musicbrainzId },
			query: { kind }
		}),
	reject: (musicbrainzId: string, kind: RequestKind = 'album') =>
		v3('/api/v3/requests/approvals/{musicbrainz_id}/reject', {
			path: { musicbrainz_id: musicbrainzId },
			query: { kind }
		})
} as const;
