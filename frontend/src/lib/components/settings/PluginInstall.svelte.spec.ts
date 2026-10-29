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
});
