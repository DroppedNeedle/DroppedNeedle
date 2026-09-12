import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	schedule: {
		data: {
			scan_frequency: '24hr',
			daily_scan_time: '03:00',
			server_timezone: '',
			last_scan: null,
			last_scan_success: true
		}
	} as Record<string, unknown>,
	save: { mutateAsync: vi.fn(), isPending: false }
}));

vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryScanScheduleQuery: () => h.schedule
}));
vi.mock('$lib/queries/library/LibraryMutations.svelte', () => ({
	saveLibraryScanSchedule: () => h.save
}));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));

import LibraryScanScheduleControl from './LibraryScanScheduleControl.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.schedule = {
		data: {
			scan_frequency: '24hr',
			daily_scan_time: '03:00',
			server_timezone: '',
			last_scan: null,
			last_scan_success: true
		}
	};
});

describe('LibraryScanScheduleControl', () => {
	it('labels the control as the scheduled frequency', async () => {
		await render(LibraryScanScheduleControl);
		await expect.element(page.getByText('Scheduled scan frequency')).toBeVisible();
		await expect.element(page.getByText('Automatic scan frequency')).not.toBeInTheDocument();
	});

	it('seeds the select from the saved schedule', async () => {
		await render(LibraryScanScheduleControl);
		await expect.element(page.getByLabelText('Scan frequency')).toHaveValue('24hr');
	});
});
