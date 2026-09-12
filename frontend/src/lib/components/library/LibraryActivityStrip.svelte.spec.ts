import { cdp, page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type {
	LibraryActivityResponse,
	LibraryWorkItem
} from '$lib/queries/library/LibraryOperationsTypes';
import '../../../app.css';

interface EmulationCdpSession {
	send(
		method: 'Emulation.setEmulatedMedia',
		params: { features: { name: string; value: string }[] }
	): Promise<unknown>;
}

const h = vi.hoisted(() => ({
	query: { data: undefined } as { data: LibraryActivityResponse | undefined },
	userId: 'user-1',
	isAdmin: true
}));

vi.mock('$lib/queries/library/LibraryActivityQueries.svelte', () => ({
	getLibraryActivityQuery: () => h.query
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get user() {
			return { id: h.userId };
		},
		get isAdmin() {
			return h.isAdmin;
		}
	}
}));

import LibraryActivityStrip from './LibraryActivityStrip.svelte';

function work(overrides: Partial<LibraryWorkItem> = {}): LibraryWorkItem {
	return {
		id: 'scan-1',
		kind: 'scan',
		state: 'running',
		phase: 'indexing',
		mode: null,
		effect: 'catalog_only',
		processed: 42,
		total: 100,
		unit: 'files',
		indeterminate: false,
		remaining_count: null,
		subject_count: null,
		started_at: 900,
		updated_at: 1_000,
		origin: null,
		profile_name: null,
		scope_label: 'Whole library',
		new_count: 0,
		changed_count: 0,
		missing_count: 0,
		warning_count: 0,
		blocked_count: 0,
		succeeded_count: 0,
		failed_count: 0,
		skipped_count: 0,
		priority: 20,
		failure_event_id: null,
		failure_at: null,
		...overrides
	};
}

async function renderStrip(
	workItems: LibraryWorkItem[],
	props: { now?: number; adminOverride?: boolean; userIdOverride?: string } = {}
) {
	return await render(LibraryActivityStrip, {
		props: {
			activityOverride: { items: [], work_items: workItems },
			now: props.now ?? 1_000,
			...props
		}
	} as unknown as Parameters<typeof render>[1]);
}

beforeEach(async () => {
	await page.viewport(1280, 720);
	localStorage.clear();
	h.isAdmin = true;
	h.userId = 'user-1';
});

describe('LibraryActivityStrip', () => {
	it('stays out of the way while every worker is idle', async () => {
		await renderStrip([]);
		await expect.element(page.getByTestId('library-activity-strip')).not.toBeInTheDocument();
	});

	it('leads with one truthful scan progress bar', async () => {
		await renderStrip([work()]);

		await expect.element(page.getByText('Scanning library', { exact: true })).toBeVisible();
		await expect.element(page.getByText('42 / 100 files · 42%')).toBeVisible();
		await expect.element(page.getByText('Reading file metadata')).toBeVisible();
		await expect
			.element(page.getByRole('progressbar', { name: 'Reading file metadata: 42 / 100 files' }))
			.toHaveAttribute('aria-valuenow', '42');
	});

	it('shows scan reconciliation as a completed file bar with a finalizing phase', async () => {
		await renderStrip([work({ phase: 'reconciling', processed: 100 })]);

		await expect.element(page.getByText('Finalizing the catalog')).toBeVisible();
		await expect.element(page.getByText('100 / 100 files · 100%')).toBeVisible();
	});

	it.each([
		['pausing', 'Pausing after the current file'],
		['stopping', 'Stopping after the current file']
	] as const)('keeps the %s transition visible', async (state, label) => {
		await renderStrip([work({ state })]);
		await expect.element(page.getByText(label)).toBeVisible();
	});

	it('uses the primary item and keeps concurrent work in an expandable stack', async () => {
		await renderStrip([
			work({
				id: 'management-1',
				kind: 'library_management',
				effect: 'file_writing',
				priority: 10
			}),
			work({ id: 'scan-1', priority: 20 })
		]);

		await expect
			.element(page.getByText('Writing tags and organizing files', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Show 1 other task' })).toBeVisible();
		await expect
			.element(page.getByText('Scanning library', { exact: true }))
			.not.toBeInTheDocument();

		await page.getByRole('button', { name: 'Show 1 other task' }).click();
		await expect.element(page.getByText('Scanning library', { exact: true })).toBeVisible();
	});

	it('does not turn historical identification totals into a misleading percentage', async () => {
		await renderStrip([
			work({
				id: 'identification',
				kind: 'identification',
				phase: 'identifying_albums',
				processed: 0,
				total: null,
				unit: 'albums',
				indeterminate: true,
				remaining_count: 7,
				priority: 90
			})
		]);

		await expect.element(page.getByText('7 albums remaining')).toBeVisible();
		await expect.element(page.getByText(/93 \/ 93/)).not.toBeInTheDocument();
		await expect.element(page.getByRole('progressbar')).not.toHaveAttribute('aria-valuenow');
	});

	it('links administrators to exact operation details and users to the library', async () => {
		const management = work({
			id: 'operation 1',
			kind: 'library_management',
			effect: 'file_writing'
		});
		await renderStrip([management], { adminOverride: true });
		await expect
			.element(page.getByRole('link'))
			.toHaveAttribute('href', '/library/management/operations/operation%201');

		await renderStrip([management], { adminOverride: false });
		await expect.element(page.getByRole('link').last()).toHaveAttribute('href', '/library');
	});

	it('persists dismissible failures per user but keeps recovery visible', async () => {
		const failed = work({
			id: 'failed-1',
			state: 'failed',
			effect: 'attention',
			priority: 0,
			failure_event_id: 'failure-1',
			failure_at: 9_900
		});
		await renderStrip([failed], { now: 10_000 });
		await page.getByRole('button', { name: 'Dismiss library failure' }).click();
		expect(localStorage.getItem('droppedneedle:library-failure:user-1:failure-1')).toBe('1');
		await expect.element(page.getByTestId('library-activity-strip')).not.toBeInTheDocument();

		await renderStrip(
			[
				work({
					id: 'recovery',
					kind: 'recovery',
					state: 'failed',
					effect: 'attention',
					priority: 0,
					failure_event_id: 'recovery:1',
					failure_at: 9_950
				})
			],
			{ now: 10_000 }
		);
		await expect
			.element(page.getByText('File recovery needs attention', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Dismiss library failure' }))
			.not.toBeInTheDocument();
	});

	it('expires a terminal failure after 24 hours', async () => {
		await renderStrip(
			[
				work({
					state: 'failed',
					effect: 'attention',
					failure_event_id: 'old-failure',
					failure_at: 1_000
				})
			],
			{ now: 1_000 + 24 * 60 * 60 }
		);
		await expect.element(page.getByTestId('library-activity-strip')).not.toBeInTheDocument();
	});

	it('expires a visible failure while the page remains open', async () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date(1_000_000 * 1000));
		let unmount: (() => Promise<void>) | undefined;
		try {
			({ unmount } = await render(LibraryActivityStrip, {
				props: {
					activityOverride: {
						items: [],
						work_items: [
							work({
								state: 'failed',
								effect: 'attention',
								failure_event_id: 'failure-live-expiry',
								failure_at: 1_000_000 - 24 * 60 * 60 + 30
							})
						]
					}
				}
			} as unknown as Parameters<typeof render>[1]));
			await expect.element(page.getByTestId('library-activity-strip')).toBeVisible();
			await vi.advanceTimersByTimeAsync(60_000);
			await expect.element(page.getByTestId('library-activity-strip')).not.toBeInTheDocument();
		} finally {
			await unmount?.();
			vi.useRealTimers();
		}
	});

	it('removes progress motion when the browser requests reduced motion', async () => {
		const session = cdp() as EmulationCdpSession;
		await session.send('Emulation.setEmulatedMedia', {
			features: [{ name: 'prefers-reduced-motion', value: 'reduce' }]
		});
		try {
			await renderStrip([work()]);
			const fill = page.getByTestId('library-work-progress-fill');
			await expect.element(fill).toBeVisible();
			expect(getComputedStyle(fill.element()).transitionDuration).toBe('0s');
		} finally {
			await session.send('Emulation.setEmulatedMedia', {
				features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }]
			});
		}
	});
});
