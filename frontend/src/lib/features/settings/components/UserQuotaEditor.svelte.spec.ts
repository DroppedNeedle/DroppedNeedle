import { page, userEvent } from 'vitest/browser';
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

vi.mock('$lib/features/settings/queries/UserQuotaQueries.svelte', () => ({
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
	createSaveUserQuotaMutation: () => ({
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

	it('saves the numbers typed, and null for a blank field so it inherits the default', async () => {
		await render(UserQuotaEditor, { props: { userId: 'u-7', displayName: 'Jae' } });

		await userEvent.fill(page.getByLabelText('Window (days)'), '14');
		await page.getByRole('button', { name: 'Save' }).click();
		expect(h.save).toHaveBeenLastCalledWith({
			userId: 'u-7',
			override: { request_quota_count: 10, request_quota_days: 14, storage_quota_gb: 50 }
		});
		expect(h.toast).toHaveBeenCalledWith({ message: 'Quota saved for Jae', type: 'success' });

		await userEvent.fill(page.getByLabelText('Requests'), '');
		await userEvent.fill(page.getByLabelText('Window (days)'), '');
		await userEvent.fill(page.getByLabelText('Storage (GB)'), '');
		await page.getByRole('button', { name: 'Save' }).click();
		expect(h.save).toHaveBeenLastCalledWith({
			userId: 'u-7',
			override: { request_quota_count: null, request_quota_days: null, storage_quota_gb: null }
		});
	});
});
