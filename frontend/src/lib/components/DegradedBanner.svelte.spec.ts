import { page } from '@vitest/browser/context';
import { describe, expect, it, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import DegradedBanner from './DegradedBanner.svelte';
import { serviceStatusStore } from '$lib/stores/serviceStatus';

async function renderBanner() {
	return await render(DegradedBanner);
}

describe('DegradedBanner.svelte', () => {
	beforeEach(() => {
		serviceStatusStore.clear();
	});

	it('is hidden when store is empty', async () => {
		expect.assertions(1);
		await renderBanner();
		await expect.element(page.getByRole('status')).not.toBeInTheDocument();
	});

	it('renders when store has degraded services', async () => {
		expect.assertions(1);
		serviceStatusStore.recordFromResponse({ musicbrainz: 'error' });
		await renderBanner();
		await expect
			.element(page.getByText(/Musicbrainz is unavailable, so some results may be missing/))
			.toBeVisible();
	});

	it('shows multiple degraded sources with plural verb', async () => {
		expect.assertions(1);
		serviceStatusStore.recordFromResponse({ musicbrainz: 'error', audiodb: 'degraded' });
		await renderBanner();
		await expect
			.element(
				page.getByText(/Musicbrainz, Audiodb are unavailable, so some results may be missing/)
			)
			.toBeVisible();
	});

	it('can be dismissed', async () => {
		expect.assertions(2);
		serviceStatusStore.recordFromResponse({ musicbrainz: 'error' });
		await renderBanner();

		await expect.element(page.getByRole('status')).toBeVisible();

		await page.getByLabelText('Dismiss').click();

		await expect.element(page.getByRole('status')).not.toBeInTheDocument();
	});
});
