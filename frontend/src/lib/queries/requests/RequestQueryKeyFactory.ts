import { key, userScope } from '../keys';
import type { HistoryParams } from './endpoints';

type UserId = string | null | undefined;

// Every key is the caller's and nests under `all(userId)`, so one prefix
// invalidation sweeps the whole request surface after any mutation.
export const RequestQueryKeyFactory = {
	feature: 'requests',
	all: (userId: UserId) => key(userScope(userId), 'requests'),
	active: (userId: UserId) => [...RequestQueryKeyFactory.all(userId), 'active'] as const,
	activeCount: (userId: UserId) => [...RequestQueryKeyFactory.all(userId), 'active-count'] as const,
	history: (userId: UserId, params: HistoryParams) =>
		[
			...RequestQueryKeyFactory.all(userId),
			'history',
			params.page ?? 1,
			params.pageSize ?? 20,
			params.status ?? '',
			params.sort ?? '',
			params.kind ?? ''
		] as const,
	approvals: (userId: UserId) => [...RequestQueryKeyFactory.all(userId), 'approvals'] as const
};
