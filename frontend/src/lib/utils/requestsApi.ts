import { authStore } from '$lib/stores/authStore.svelte';
import { FollowQueryKeyFactory } from '$lib/queries/following/FollowQueryKeyFactory';
import {
	invalidateQueriesWithPersister,
	setQueryDataWithPersister
} from '$lib/queries/QueryClient';

export function notifyPendingApprovalCountChanged(count?: number): void {
	const queryKey = FollowQueryKeyFactory.pendingApprovalCount(authStore.user?.id);
	if (typeof count === 'number') {
		void setQueryDataWithPersister<{ count: number }>(queryKey, { count });
		return;
	}
	void invalidateQueriesWithPersister({ queryKey, exact: true });
}
