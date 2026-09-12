import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	goto: vi.fn(),
	appPage: {
		url: new URL('https://music.example.test/library/management'),
		state: {}
	},
	settings: {
		data: {
			library_roots: [
				{
					id: 'root-1',
					path: '/music',
					label: 'Music',
					policy: 'automatic',
					rules: []
				}
			],
			enabled: true,
			policy_revision: 'policy-1'
		},
		isLoading: false,
		isError: false
	} as Record<string, unknown>,
	activity: {
		data: { items: [], work_items: [] as Array<Record<string, unknown>> },
		isLoading: false,
		isError: false
	},
	scanningRender: vi.fn(),
	organizeRender: vi.fn(),
	overviewRender: vi.fn(),
	settingsRender: vi.fn()
}));

vi.mock('$app/navigation', () => ({ goto: h.goto }));
vi.mock('$app/state', () => ({ page: h.appPage }));

vi.mock('$lib/components/library/LibraryScanningPanel.svelte', () => {
	const Comp = function () {
		h.scanningRender();
	};
	Comp.prototype = {};
	return { default: Comp };
});

vi.mock('$lib/components/library/LibraryManagementControlRoom.svelte', () => {
	const Comp = function () {
		h.organizeRender();
	};
	Comp.prototype = {};
	return { default: Comp };
});

vi.mock('$lib/components/library/LibraryOverviewPanel.svelte', () => {
	const Comp = function () {
		h.overviewRender();
	};
	Comp.prototype = {};
	return { default: Comp };
});

vi.mock('$lib/components/settings/SettingsLibraryManagement.svelte', () => {
	const Comp = function (_anchor: unknown, props: Record<string, unknown>) {
		h.settingsRender(props);
	};
	Comp.prototype = {};
	return { default: Comp };
});

vi.mock('$lib/queries/library/LibraryPolicyQueries.svelte', () => ({
	getTargetLibrarySettingsQuery: () => h.settings
}));

vi.mock('$lib/queries/library/LibraryActivityQueries.svelte', () => ({
	getLibraryActivityQuery: () => h.activity
}));

import LibraryManagementPage from './+page.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.appPage.url = new URL('https://music.example.test/library/management');
	h.activity.data.work_items = [];
});

describe('Library Management route page', () => {
	it('defaults to the Overview tab and keeps History reachable', async () => {
		await render(LibraryManagementPage);
		await expect.element(page.getByRole('heading', { name: 'Library Management' })).toBeVisible();
		await expect
			.element(page.getByRole('tab', { name: 'Overview' }))
			.toHaveAttribute('aria-selected', 'true');
		await expect
			.element(page.getByRole('tab', { name: 'Scanning' }))
			.toHaveAttribute('aria-selected', 'false');
		await expect
			.element(page.getByRole('tab', { name: 'Organize files' }))
			.toHaveAttribute('aria-selected', 'false');
		await expect
			.element(page.getByRole('tab', { name: 'Automation' }))
			.toHaveAttribute('aria-selected', 'false');
		await expect
			.element(page.getByRole('tab', { name: 'Organization history' }))
			.toHaveAttribute('href', '/library/management/history');
		await expect
			.element(page.getByRole('tab', { name: 'Overview' }))
			.toHaveAttribute('aria-controls', 'management-panel-overview');
		await expect
			.element(page.getByRole('tabpanel'))
			.toHaveAttribute('aria-labelledby', 'management-tab-overview');
		expect(h.overviewRender).toHaveBeenCalledOnce();
		expect(h.scanningRender).not.toHaveBeenCalled();
		expect(h.organizeRender).not.toHaveBeenCalled();
		expect(h.settingsRender).not.toHaveBeenCalled();
	});

	it('writes the chosen tab to the URL without scrolling', async () => {
		await render(LibraryManagementPage);
		await page.getByRole('tab', { name: 'Organize files' }).click();
		expect(h.goto).toHaveBeenCalledOnce();
		const [url, options] = h.goto.mock.calls[0] as [URL, Record<string, unknown>];
		expect(url.searchParams.get('tab')).toBe('organize');
		expect(url.hash).toBe('');
		expect(options).toMatchObject({ replaceState: true, noScroll: true, keepFocus: true });
	});

	it('routes the runner parameter to the Organize tab', async () => {
		h.appPage.url = new URL('https://music.example.test/library/management?runner=manage');
		await render(LibraryManagementPage);
		await vi.waitFor(() => expect(h.goto).toHaveBeenCalled());
		const [url] = h.goto.mock.calls[0] as [URL, Record<string, unknown>];
		expect(url.searchParams.get('tab')).toBe('organize');
		expect(url.searchParams.get('runner')).toBe('manage');
	});

	it.each([
		['#management-controls', 'organize'],
		['#identity-readiness', 'organize'],
		['#management-settings', 'automation'],
		['#recent-runs', 'scanning']
	])('maps legacy link %s to the %s tab', async (hash, tab) => {
		h.appPage.url = new URL(`https://music.example.test/library/management${hash}`);
		await render(LibraryManagementPage);
		await vi.waitFor(() => expect(h.goto).toHaveBeenCalled());
		const [url] = h.goto.mock.calls[0] as [URL, Record<string, unknown>];
		expect(url.searchParams.get('tab')).toBe(tab);
	});

	it('routes the scanning legacy link to the Scanning tab', async () => {
		h.appPage.url = new URL('https://music.example.test/library/management#scanning-controls');
		await render(LibraryManagementPage);
		await expect.element(page.getByRole('tab', { name: 'Scanning' })).toBeVisible();
		await vi.waitFor(() => expect(h.goto).toHaveBeenCalled());
		const [url] = h.goto.mock.calls[0] as [URL, Record<string, unknown>];
		expect(url.searchParams.get('tab')).toBe('scanning');
	});

	it('mounts automation settings with the saved roots and policy revision on that tab', async () => {
		h.appPage.url = new URL('https://music.example.test/library/management?tab=automation');
		await render(LibraryManagementPage);
		await vi.waitFor(() => expect(h.settingsRender).toHaveBeenCalled());
		expect(h.settingsRender).toHaveBeenCalledWith(
			expect.objectContaining({
				roots: expect.arrayContaining([expect.objectContaining({ id: 'root-1' })]),
				policyRevision: 'policy-1'
			})
		);
		expect(h.scanningRender).not.toHaveBeenCalled();
	});

	it('shows disabled states for organize and automation when the library is off', async () => {
		h.settings = {
			data: { ...(h.settings.data as Record<string, unknown>), enabled: false },
			isLoading: false,
			isError: false
		};
		h.appPage.url = new URL('https://music.example.test/library/management?tab=organize');
		await render(LibraryManagementPage);
		await expect.element(page.getByText('The local library is disabled').first()).toBeVisible();
		expect(h.organizeRender).not.toHaveBeenCalled();
		h.appPage.url = new URL('https://music.example.test/library/management?tab=automation');
		await render(LibraryManagementPage);
		await expect.element(page.getByText('The local library is disabled').nth(1)).toBeVisible();
		expect(h.settingsRender).not.toHaveBeenCalled();
	});

	it('shows live-work badges on the matching tab labels', async () => {
		h.activity.data.work_items = [
			{
				id: 'scan-1',
				kind: 'scan',
				state: 'running',
				phase: 'indexing',
				effect: 'catalog_only',
				processed: 50,
				total: 100,
				unit: 'files',
				indeterminate: false,
				remaining_count: null
			},
			{
				id: 'management-1',
				kind: 'library_management',
				state: 'running',
				phase: 'applying',
				effect: 'file_writing',
				processed: 1,
				total: 4,
				unit: 'releases',
				indeterminate: false,
				remaining_count: null
			}
		];

		await render(LibraryManagementPage);

		await expect
			.element(page.getByRole('tab', { name: /Scanning/ }).getByText('50%'))
			.toBeVisible();
		await expect
			.element(page.getByRole('tab', { name: /Organize files/ }).getByText('Writing'))
			.toBeVisible();
	});
});
