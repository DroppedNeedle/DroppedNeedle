import { page, userEvent } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	data: null as null | {
		exempt: boolean;
		requests_in_window: number;
		effective_request_quota_count: number;
		effective_request_quota_days: number;
		effective_storage_quota_gb: number;
		storage_bytes: number;
		user_id: string;
		quota_override: {
			request_quota_count: number | null;
			request_quota_days: number | null;
			storage_quota_gb: number | null;
		};
	},
	isPending: false,
	isError: false,
	save: vi.fn().mockResolvedValue(undefined),
	savePending: false,
	toast: vi.fn()
}));

vi.mock('$lib/queries/auth/UserQuotaQueries.svelte', () => ({
	getUserQuotaQuery: () => ({
		get data() {
			return h.data;
		},
		get isPending() {
			return h.isPending;
		},
		get isError() {
			return h.isError;
		}
	}),
	saveUserQuota: () => ({
		mutateAsync: h.save,
		get isPending() {
			return h.savePending;
		}
	})
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: (...args: unknown[]) => h.toast(...args) }
}));

import UserQuotaEditor from './UserQuotaEditor.svelte';

function quotaData() {
	return {
		exempt: false,
		requests_in_window: 3,
		effective_request_quota_count: 10,
		effective_request_quota_days: 7,
		effective_storage_quota_gb: 50,
		storage_bytes: 5 * 1024 ** 3,
		user_id: 'u-7',
		quota_override: { request_quota_count: 10, request_quota_days: null, storage_quota_gb: 50 }
	};
}

describe('UserQuotaEditor', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		h.data = quotaData();
		h.isPending = false;
		h.isError = false;
		h.savePending = false;
	});

	it('summarizes usage and seeds inputs from the stored override', async () => {
		await render(UserQuotaEditor, { props: { userId: 'u-7', displayName: 'Jae' } });

		await expect.element(page.getByText(/requests: 3/i)).toBeVisible();
		await expect.element(page.getByText(/downloads: 5\.0 gb/i)).toBeVisible();
		await expect.element(page.getByLabelText('Requests')).toHaveValue(10);
		await expect.element(page.getByLabelText('Storage (GB)')).toHaveValue(50);
		await expect.element(page.getByLabelText('Window (days)')).toHaveValue(null);
	});

	it('saves parsed numbers and confirms with a toast', async () => {
		await render(UserQuotaEditor, { props: { userId: 'u-7', displayName: 'Jae' } });

		await userEvent.fill(page.getByLabelText('Window (days)'), '14');
		await page.getByRole('button', { name: 'Save' }).click();

		expect(h.save).toHaveBeenCalledWith({
			userId: 'u-7',
			override: { request_quota_count: 10, request_quota_days: 14, storage_quota_gb: 50 }
		});
		expect(h.toast).toHaveBeenCalledWith({ message: 'Quota saved for Jae', type: 'success' });
	});

	it('sends null for blank fields so they inherit the global default', async () => {
		await render(UserQuotaEditor, { props: { userId: 'u-7', displayName: 'Jae' } });

		await userEvent.fill(page.getByLabelText('Requests'), '');
		await userEvent.fill(page.getByLabelText('Window (days)'), '');
		await userEvent.fill(page.getByLabelText('Storage (GB)'), '');
		await page.getByRole('button', { name: 'Save' }).click();

		expect(h.save).toHaveBeenCalledWith({
			userId: 'u-7',
			override: { request_quota_count: null, request_quota_days: null, storage_quota_gb: null }
		});
	});

	it('says plainly when the quota cannot be loaded', async () => {
		h.data = null;
		h.isError = true;

		await render(UserQuotaEditor, { props: { userId: 'u-7', displayName: 'Jae' } });

		await expect.element(page.getByText(/could not load this user's quota/i)).toBeVisible();
	});
});
