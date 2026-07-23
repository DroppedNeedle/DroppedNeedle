import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
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
			policy_revision: 'policy-1'
		},
		isLoading: false,
		isError: false
	} as Record<string, unknown>,
	operationsRender: vi.fn(),
	settingsRender: vi.fn()
}));

vi.mock('$lib/components/library/LibraryOperationsPanel.svelte', () => {
	const Comp = function () {
		h.operationsRender();
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

import LibraryManagementPage from './+page.svelte';

beforeEach(() => vi.clearAllMocks());

describe('Library Management route page', () => {
	it('presents one administrator workspace with clear scan and write destinations', async () => {
		render(LibraryManagementPage);
		await expect.element(page.getByRole('heading', { name: 'Library Management' })).toBeVisible();
		const navigation = page.getByRole('navigation', { name: 'Library Management sections' });
		await expect
			.element(navigation.getByRole('link', { name: 'Scan & identify' }))
			.toHaveAttribute('href', '#scanning-controls');
		await expect
			.element(navigation.getByRole('link', { name: 'Manage files' }))
			.toHaveAttribute('href', '#management-controls');
		await expect
			.element(navigation.getByRole('link', { name: 'Profiles & automation' }))
			.toHaveAttribute('href', '#management-settings');
		await expect
			.element(navigation.getByRole('link', { name: 'History' }))
			.toHaveAttribute('href', '/library/management/history');
		expect(h.operationsRender).toHaveBeenCalledOnce();
	});

	it('mounts profile and automation settings with the saved roots and policy revision', async () => {
		render(LibraryManagementPage);
		await expect
			.element(page.getByRole('heading', { name: 'Profiles & automation' }))
			.toBeVisible();
		expect(h.settingsRender).toHaveBeenCalledWith(
			expect.objectContaining({
				roots: expect.arrayContaining([expect.objectContaining({ id: 'root-1' })]),
				policyRevision: 'policy-1'
			})
		);
	});
});
