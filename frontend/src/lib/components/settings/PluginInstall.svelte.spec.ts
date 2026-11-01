import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { apiError, fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import PluginInstall from './PluginInstall.svelte';

describe('installing a plugin while GitHub requests are paced', () => {
	it('says when to try again, in minutes', async () => {
		fakeApi({
			'POST /api/v3/plugins/install/preview': apiError(
				503,
				'UPSTREAM_RATE_LIMITED',
				'GitHub is rate limiting this server, try again later',
				{ retry_after: 144 }
			)
		});
		await renderWithQuery(PluginInstall, {});

		await page.getByLabelText('Install from GitHub').fill('https://github.com/acme/toy');
		await page.getByRole('button', { name: 'Preview' }).click();

		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent('GitHub requests are paced. Try again in 3 minutes.');
	});

	it('installs with the reference the preview showed', async () => {
		const api = fakeApi({
			'POST /api/v3/plugins/install/preview': {
				name: 'toy',
				display_name: 'Toy',
				version: '1.0.0',
				description: '',
				author: '',
				homepage: '',
				capabilities: [],
				permissions: [],
				repository: 'acme/toy',
				ref_kind: 'release',
				reference: 'v1.0.0',
				commit: '0123456789abcdef0123456789abcdef01234567',
				warning: 'Only install plugins you trust.'
			} as never,
			'POST /api/v3/plugins/install': { display_name: 'Toy' } as never
		});
		await renderWithQuery(PluginInstall, {});

		await page.getByLabelText('Install from GitHub').fill('https://github.com/acme/toy');
		await page.getByRole('button', { name: 'Preview' }).click();
		await page.getByRole('button', { name: 'Install' }).click();

		await expect.poll(() => api.calls('POST /api/v3/plugins/install').length).toBe(1);
		expect(api.calls('POST /api/v3/plugins/install')[0].body).toEqual({
			repository_url: 'https://github.com/acme/toy',
			commit: '0123456789abcdef0123456789abcdef01234567',
			reference: 'v1.0.0',
			ref_kind: 'release'
		});
	});
});
