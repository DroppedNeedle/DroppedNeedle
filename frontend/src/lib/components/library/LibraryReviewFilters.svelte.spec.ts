import { page } from '@vitest/browser/context';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LibraryReviewFilters from './LibraryReviewFilters.svelte';

const roots = [
	{
		id: 'root-1',
		kind: 'root' as const,
		label: 'Main library',
		path: '/music',
		policy: 'automatic' as const,
		inherited_from_id: 'root-1',
		available: true,
		indexed_file_count: 10,
		on_disk_file_count: 10,
		children: []
	}
];

afterEach(async () => {
	await page.viewport(1280, 720);
});

describe('LibraryReviewFilters', () => {
	it('offers root and reason filters on desktop', async () => {
		const onchange = vi.fn();
		await render(LibraryReviewFilters, {
			props: { filters: {}, roots, onchange }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('combobox', { name: 'Library root' }).selectOptions('root-1');
		await page.getByRole('combobox', { name: 'Review reason' }).selectOptions('AMBIGUOUS');
		await page.getByRole('combobox', { name: 'Review state' }).selectOptions('');

		expect(onchange).toHaveBeenCalledWith(expect.objectContaining({ rootId: 'root-1' }));
		expect(onchange).toHaveBeenCalledWith(expect.objectContaining({ reasonCode: 'AMBIGUOUS' }));
		expect(onchange).toHaveBeenCalledWith(expect.objectContaining({ state: undefined }));
	});

	it('includes root and reason controls in the mobile dialog', async () => {
		await page.viewport(390, 760);
		await render(LibraryReviewFilters, {
			props: { filters: {}, roots, onchange: vi.fn() }
		} as unknown as Parameters<typeof render>[1]);

		const opener = page.getByRole('button', { name: 'Filters' });
		await opener.click();
		const dialog = page.getByRole('dialog', { name: 'Review filters' });
		await expect.element(dialog.getByRole('heading', { name: 'Review filters' })).toHaveFocus();
		await expect.element(dialog.getByText('Reason', { exact: true })).toBeVisible();
		await expect.element(dialog.getByText('Library root', { exact: true }).first()).toBeVisible();
		await expect.element(dialog.getByText('Main library')).toBeInTheDocument();
		await dialog.getByRole('button', { name: 'Done' }).click();
		await expect.element(opener).toHaveFocus();
	});
});
