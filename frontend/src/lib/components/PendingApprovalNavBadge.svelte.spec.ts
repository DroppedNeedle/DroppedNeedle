import { page } from '@vitest/browser/context';
import { expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

vi.mock('$lib/queries/following/AdminApprovalsQueries.svelte', () => ({
	getPendingApprovalCountQuery: () => ({ data: { count: 12 } })
}));

import PendingApprovalNavBadge from './PendingApprovalNavBadge.svelte';

it('caps the visible count while preserving the exact accessible label', async () => {
	await render(PendingApprovalNavBadge);

	await expect.element(page.getByLabelText('12 pending approvals')).toHaveTextContent('9+');
});
