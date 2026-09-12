import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	securityData: {
		hibp_check: true,
		hibp_local_path: '',
		hsts_max_age: 0,
		hsts_include_subdomains: false,
		hsts_preload: false,
		library_download_access: 'everyone'
	},
	save: vi.fn(),
	load: vi.fn(),
	cleanup: vi.fn()
}));

vi.mock('$lib/utils/settingsForm.svelte', () => ({
	createSettingsForm: (opts: { loadEndpoint: string }) => {
		if (opts.loadEndpoint === '/api/v1/settings/security') {
			return {
				data: h.securityData,
				loading: false,
				saving: false,
				message: '',
				messageType: 'success',
				testResult: null,
				wasAlreadyEnabled: false,
				load: h.load,
				save: h.save,
				test: vi.fn(),
				cleanup: h.cleanup
			};
		}
		// OIDC card stays empty: only the security form is under test.
		return {
			data: null,
			loading: false,
			saving: false,
			testing: false,
			message: '',
			messageType: 'success',
			testResult: null,
			wasAlreadyEnabled: false,
			load: vi.fn(),
			save: vi.fn(),
			test: vi.fn(),
			cleanup: vi.fn()
		};
	}
}));

import SettingsSecurity from './SettingsSecurity.svelte';

describe('SettingsSecurity library downloads', () => {
	beforeEach(() => {
		h.securityData.library_download_access = 'everyone';
		h.save.mockClear();
	});

	it('reflects the loaded access level', async () => {
		expect.assertions(2);
		h.securityData.library_download_access = 'trusted';
		await render(SettingsSecurity);

		await expect.element(page.getByText('Library downloads')).toBeVisible();
		await expect
			.element(page.getByRole('radio', { name: /Trusted users and admins/ }))
			.toBeChecked();
	});

	it('saves the selected access level', async () => {
		expect.assertions(2);
		await render(SettingsSecurity);

		await page.getByText('Admins only').click();
		await page.getByRole('button', { name: 'Save Settings' }).click();
		expect(h.save).toHaveBeenCalledTimes(1);
		expect(h.securityData.library_download_access).toBe('admin');
	});
});
