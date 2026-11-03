import type { V3QueryOf } from '$lib/api/v3/endpoint';
import type { RequestKind } from './types';

// v3 names the kind filter `kind`.
export interface HistoryParams {
	page?: number;
	pageSize?: number;
	status?: string;
	sort?: string;
	kind?: RequestKind;
}

export function historyQuery(params: HistoryParams): V3QueryOf<'/api/v3/requests/history'> {
	const query: V3QueryOf<'/api/v3/requests/history'> = {};
	if (params.page !== undefined) query.page = params.page;
	if (params.pageSize !== undefined) query.page_size = params.pageSize;
	if (params.status) query.status = params.status;
	if (params.sort) query.sort = params.sort;
	if (params.kind) query.kind = params.kind;
	return query;
}
