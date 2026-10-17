import { test, expect, mocks, setUpAdmin } from './fixtures';

// A request made while no download source is set up is held, not refused.
// Saving a source wakes the download worker, so the request starts at once
// instead of at the worker's next pass.
test('a request made with no download source waits, then starts when one is saved', async ({
	page
}) => {
	await setUpAdmin(page);

	// MusicBrainz answers from the mock, unpaced.
	await page.goto('/settings?tab=musicbrainz');
	await page.getByRole('radio', { name: 'Self-hosted mirror' }).check();
	await page.getByLabel('Mirror API Endpoint URL').fill(mocks.musicbrainz);
	await page.getByLabel('Rate Limit (requests/sec)').fill('0');
	await page.getByRole('button', { name: 'Save Settings' }).click();
	await page.getByRole('button', { name: 'Test Connection' }).click();
	await expect(page.getByText('Connected to MusicBrainz.')).toBeVisible();

	// Search, open the artist and request its albums.
	await page.getByRole('searchbox', { name: 'Search' }).first().fill('Radiohead');
	await page.getByRole('searchbox', { name: 'Search' }).first().press('Enter');
	await page
		.getByRole('link')
		.filter({ has: page.getByRole('heading', { name: 'Radiohead', exact: true }) })
		.click();
	await expect(page.getByRole('heading', { name: 'Radiohead', level: 1 })).toBeVisible();
	await page.locator('#section-albums').getByRole('button', { name: 'Download All' }).click();
	const confirm = page.getByRole('dialog');
	await expect(confirm.getByText('OK Computer')).toBeAttached();
	await confirm.getByRole('button', { name: 'Download 2 Albums' }).click();
	await expect(confirm).toBeHidden();

	// Held, and the page says why.
	await page.goto('/requests');
	const okComputer = page.locator('.card', {
		has: page.getByRole('link', { name: 'Open OK Computer', exact: true })
	});
	await expect(okComputer.getByLabel('Status: Waiting')).toBeVisible();
	await expect(okComputer.getByText('Waiting: no download source configured')).toBeVisible();

	// The admin adds the mock slskd and turns it on.
	await page.goto('/settings?tab=download-client');
	await page.getByRole('button', { name: /^slskd/ }).click();
	await page.getByLabel('slskd URL').fill(mocks.slskd);
	await page.getByLabel('API key', { exact: true }).fill('journey-key');
	await page.getByRole('button', { name: 'Save settings' }).click();
	await expect(page.getByText('Soulseek settings saved')).toBeVisible();
	await page.getByRole('checkbox', { name: 'Enable slskd download client' }).check();
	await expect(page.getByText('Soulseek enabled')).toBeVisible();

	// The card stops waiting as soon as a source is on.
	await page.goto('/requests');
	await expect(okComputer.getByText('Waiting: no download source configured')).toBeHidden();
	await expect(okComputer.getByLabel('Status: Downloading')).toBeVisible();

	// The worker searches the mock slskd and holds the copies it finds for a
	// pick, which the album page offers. It does so well inside its own 30 s
	// pass, so the save woke it.
	// The link covers the card; its middle holds the artist link.
	await okComputer
		.getByRole('link', { name: 'Open OK Computer', exact: true })
		.click({ position: { x: 8, y: 8 } });
	await expect(page.getByText('Awaiting review', { exact: true })).toBeVisible({
		timeout: 10_000
	});
});
