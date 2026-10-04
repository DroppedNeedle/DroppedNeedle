import { userIdSegment } from '../userKeySegment';
import type { HistoryParams } from './endpoints';

// userId scopes every key: the cache persists to IndexedDB across refresh on
// shared browsers, and a keyless list would leak one user's requests to
// another. Everything nests under `all` so one prefix invalidation sweeps
// the whole request surface after any mutation.
export const RequestQueryKeyFactory = {
	all: ['requests'] as const,
	active: (userId: string | null | undefined) =>
		[...RequestQueryKeyFactory.all, 'active', userIdSegment(userId)] as const,
	activeCount: (userId: string | null | undefined) =>
		[...RequestQueryKeyFactory.all, 'active-count', userIdSegment(userId)] as const,
	history: (userId: string | null | undefined, params: HistoryParams) =>
		[
			...RequestQueryKeyFactory.all,
			'history',
			userIdSegment(userId),
			params.page ?? 1,
			params.pageSize ?? 20,
			params.status ?? '',
			params.sort ?? '',
			params.kind ?? ''
		] as const,
	approvals: (userId: string | null | undefined) =>
		[...RequestQueryKeyFactory.all, 'approvals', userIdSegment(userId)] as const
};
