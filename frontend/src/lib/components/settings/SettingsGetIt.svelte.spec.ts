import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	load: vi.fn(),
	save: vi.fn(),
	cleanup: vi.fn()
}));

vi.mock('$lib/utils/settingsForm.svelte', () => ({
	createSettingsForm: () => ({
		data: { store_region: 'GB' },
		loading: false,
		saving: false,
		message: '',
		messageType: 'success',
		load: h.load,
		save: h.save,
		cleanup: h.cleanup
	})
}));

import SettingsGetIt from './SettingsGetIt.svelte';

describe('SettingsGetIt', () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	it('keeps the regional storefront setting', async () => {
		await render(SettingsGetIt);

		await expect.element(page.getByRole('combobox', { name: 'Store region' })).toHaveValue('GB');
		await page.getByRole('button', { name: 'Save' }).click();
		expect(h.save).toHaveBeenCalledOnce();
	});

	it('offers direct support links without a monetization setting', async () => {
		await render(SettingsGetIt);

		await expect
			.element(page.getByRole('link', { name: 'Ko-fi' }))
			.toHaveAttribute('href', 'https://ko-fi.com/M4M41URGJO');
		await expect
			.element(page.getByRole('link', { name: 'GitHub Sponsors' }))
			.toHaveAttribute('href', 'https://github.com/sponsors/HabiRabbu');
		await expect.element(page.getByRole('checkbox')).not.toBeInTheDocument();
	});
});
