import { cdp, page, userEvent } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type {
	LibraryActivityItem,
	LibraryActivityResponse
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

function item(
	kind: 'scan' | 'identification',
	overrides: Partial<LibraryActivityItem> = {}
): LibraryActivityItem {
	return {
		kind,
		state: 'running',
		label: kind === 'scan' ? 'Updating the local library' : 'Identifying albums',
		processed: kind === 'scan' ? 42 : 27,
		total: 100,
		indeterminate: false,
		updated_at: 1_000,
		started_at: 900,
		waiting_count: kind === 'identification' ? 73 : 0,
		identified_count: kind === 'identification' ? 27 : 0,
		kept_local_count: 0,
		needs_review_count: 0,
		failed_count: 0,
		deferred_count: 0,
		priority_band: null,
		oldest_backlog_at: null,
		provider_unavailable: false,
		control_revision: kind === 'identification' ? 1 : null,
		failure_event_id: null,
		failure_at: null,
		foreground_operation_count: 0,
		...overrides
	};
}

function renderStrip(
	items: LibraryActivityItem[],
	props: { now?: number; adminOverride?: boolean; userIdOverride?: string } = {}
) {
	return render(LibraryActivityStrip, {
		props: { activityOverride: { items }, now: props.now ?? 1_000, ...props }
	} as unknown as Parameters<typeof render>[1]);
}

beforeEach(async () => {
	await page.viewport(1280, 720);
	localStorage.clear();
	h.isAdmin = true;
	h.userId = 'user-1';
});

describe('LibraryActivityStrip', () => {
	it('renders nothing when both workloads are idle', async () => {
		renderStrip([]);
		await expect.element(page.getByTestId('library-activity-strip')).not.toBeInTheDocument();
	});

	it('keeps separate truthful lanes when both workloads are active', async () => {
		renderStrip([item('scan'), item('identification')]);
		await expect.element(page.getByText('42 of 100')).toBeVisible();
		await expect.element(page.getByText('27 of 100')).toBeVisible();
		await expect
			.element(page.getByRole('progressbar', { name: 'Local files progress' }))
			.toHaveAttribute('aria-valuenow', '42');
		await expect
			.element(page.getByRole('progressbar', { name: 'Identification progress' }))
			.toHaveAttribute('aria-valuenow', '27');
		await expect.element(page.getByText('69 of 200')).not.toBeInTheDocument();
	});

	it('keeps the idle lane position and does not invent its value', async () => {
		renderStrip([item('identification')]);
		await expect.element(page.getByText('Idle')).toBeVisible();
		await expect
			.element(page.getByRole('progressbar', { name: 'Local files progress' }))
			.not.toHaveAttribute('aria-valuenow');
	});

	it('uses indeterminate semantics when a total is not known', async () => {
		renderStrip([item('scan', { processed: 8, total: null, indeterminate: true })]);
		const lane = page.getByRole('progressbar', { name: 'Local files progress' });
		await expect.element(lane).toHaveAttribute('aria-valuetext', '8 complete, total not known yet');
		await expect.element(lane).not.toHaveAttribute('aria-valuenow');
	});

	it('reports files found while discovery is still counting the total', async () => {
		renderStrip([
			item('scan', { state: 'discovering', processed: 128, total: null, indeterminate: true })
		]);
		const lane = page.getByRole('progressbar', { name: 'Local files progress' });
		await expect.element(lane).toHaveAttribute('aria-valuetext', '128 files found');
		await expect.element(lane).not.toHaveAttribute('aria-valuenow');
	});

	it.each([
		['pausing', 'Pausing after the current file...'],
		['paused', 'Local library update paused'],
		['stopping', 'Stopping after the current file...']
	] as const)('keeps the %s scan transition visible', async (state, copy) => {
		renderStrip([item('scan', { state })]);
		await expect.element(page.getByText(copy).last()).toBeVisible();
	});

	it('announces scan state changes through a polite live region', async () => {
		renderStrip([item('scan', { state: 'pausing' })]);
		const announcement = page.getByText('Pausing after the current file...').first();
		expect(announcement.element().getAttribute('aria-live')).toBe('polite');
	});

	it('routes the one strip link by role and has no nested controls', async () => {
		renderStrip([item('scan')], { adminOverride: true });
		const link = page.getByRole('link');
		await expect.element(link).toHaveAttribute('href', '/library/management#operations');
		await userEvent.tab();
		await expect.element(link).toHaveFocus();
		await expect.element(link.getByRole('button')).not.toBeInTheDocument();

		renderStrip([item('scan')], { adminOverride: false });
		await expect.element(page.getByRole('link').last()).toHaveAttribute('href', '/library');
	});

	it('quiets a healthy identification-only backlog at the 24-hour boundary', async () => {
		const identification = item('identification', { started_at: 1_000 });
		renderStrip([identification], { now: 1_000 + 24 * 60 * 60 - 1 });
		await expect.element(page.getByText('Library identification in progress').last()).toBeVisible();

		renderStrip([identification], { now: 1_000 + 24 * 60 * 60 });
		await expect.element(page.getByText('Library identification continues').last()).toBeVisible();
		await expect.element(page.getByText(/27 complete/).last()).toBeVisible();
		await expect
			.element(page.getByRole('link').last())
			.toHaveAttribute(
				'aria-label',
				'Local files idle. Identification running, 27 complete and 73 waiting'
			);
	});

	it('persists failure dismissal per user and event while showing a new failure', async () => {
		const failed = item('identification', {
			state: 'failed',
			processed: 1,
			total: 1,
			waiting_count: 0,
			failure_event_id: 'failure-1',
			failure_at: 9_900
		});
		renderStrip([failed], { now: 10_000, userIdOverride: 'user-1' });
		await expect.element(page.getByText('Library identification needs attention')).toBeVisible();
		await page.getByRole('button', { name: 'Dismiss library failure' }).click();
		expect(localStorage.getItem('droppedneedle:library-failure:user-1:failure-1')).toBe('1');
		await expect
			.element(page.getByText('Library identification needs attention'))
			.not.toBeInTheDocument();

		renderStrip([{ ...failed, failure_event_id: 'failure-2', failure_at: 9_950 }], {
			now: 10_000,
			userIdOverride: 'user-1'
		});
		await expect
			.element(page.getByText('Library identification needs attention').last())
			.toBeVisible();
	});

	it('expands quiet identification for foreground work and handles a scan failure separately', async () => {
		renderStrip(
			[
				item('identification', {
					state: 'idle',
					waiting_count: 0,
					started_at: 1_000,
					foreground_operation_count: 1
				})
			],
			{ now: 1_000 + 25 * 60 * 60 }
		);
		await expect
			.element(page.getByText('Administrative library work in progress').last())
			.toBeVisible();
		await expect
			.element(page.getByText('Library identification continues'))
			.not.toBeInTheDocument();

		renderStrip(
			[
				item('scan', {
					state: 'failed',
					failure_event_id: 'scan-failure',
					failure_at: 10_000
				})
			],
			{ now: 10_001 }
		);
		await expect
			.element(page.getByText('Local library update needs attention').last())
			.toBeVisible();
		await page.getByRole('button', { name: 'Dismiss library failure' }).last().click();
		await expect.element(page.getByText('Local library update failed')).not.toBeInTheDocument();
	});

	it('expires a terminal failure notice after 24 hours', async () => {
		renderStrip(
			[
				item('identification', {
					state: 'failed',
					waiting_count: 0,
					failure_event_id: 'failure-old',
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
		let unmount: (() => void) | undefined;
		try {
			({ unmount } = render(LibraryActivityStrip, {
				props: {
					activityOverride: {
						items: [
							item('identification', {
								state: 'failed',
								waiting_count: 0,
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
			unmount?.();
			vi.useRealTimers();
		}
	});

	it('removes progress motion when the browser requests reduced motion', async () => {
		const session = cdp() as EmulationCdpSession;
		await session.send('Emulation.setEmulatedMedia', {
			features: [{ name: 'prefers-reduced-motion', value: 'reduce' }]
		});
		try {
			renderStrip([item('scan')]);
			const fill = page.getByTestId('scan-progress-fill');
			await expect.element(fill).toBeVisible();
			expect(getComputedStyle(fill.element()).transitionDuration).toBe('0s');
		} finally {
			await session.send('Emulation.setEmulatedMedia', {
				features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }]
			});
		}
	});
});
