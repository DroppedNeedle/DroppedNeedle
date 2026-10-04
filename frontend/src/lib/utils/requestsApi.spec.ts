import { beforeEach, describe, expect, it, vi } from 'vitest';

const { mockInvalidate, mockSet } = vi.hoisted(() => ({
	mockInvalidate: vi.fn(),
	mockSet: vi.fn()
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: (...args: unknown[]) => mockInvalidate(...args),
	setQueryDataWithPersister: (...args: unknown[]) => mockSet(...args)
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'admin-1' } }
}));

import { notifyPendingApprovalCountChanged } from './requestsApi';

describe('pending approval count notifications', () => {
	beforeEach(() => vi.clearAllMocks());

	it('invalidates the exact persisted admin-user count key', () => {
		notifyPendingApprovalCountChanged();
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: ['following', 'admin-approvals', 'count', 'admin-1'],
			exact: true
		});
	});

	it('writes an explicit count through the persister', () => {
		notifyPendingApprovalCountChanged(4);
		expect(mockSet).toHaveBeenCalledWith(['following', 'admin-approvals', 'count', 'admin-1'], {
			count: 4
		});
	});
});
