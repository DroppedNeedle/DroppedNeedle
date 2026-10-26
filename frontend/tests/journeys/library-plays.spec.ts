import { test, expect, setUpAdmin } from './fixtures';

// The script mounts a copy of two committed fixtures, Radiohead's "Airbag"
// and "Paranoid Android" from OK Computer, at /music.
test('a scanned library folder shows its album, and the album plays', async ({ page }) => {
	// Note each audio element that starts playing, and from where.
	await page.addInitScript(() => {
		const played: string[] = [];
		(window as unknown as { __played: string[] }).__played = played;
		document.addEventListener(
			'playing',
			(event) => {
				played.push((event.target as HTMLMediaElement).currentSrc);
			},
			true
		);
	});

	await setUpAdmin(page);

	await page.goto('/settings?tab=library');
	await page.getByRole('button', { name: 'Add root' }).click();
	const addRoot = page.getByRole('dialog');
	await addRoot.getByLabel('Name').fill('Journey music');
	await addRoot.getByLabel('Folder path').fill('/music');
	await addRoot.getByRole('button', { name: 'Add root', exact: true }).click();
	await page.getByRole('button', { name: 'Preview and save settings' }).click();
	await page.getByRole('dialog').getByRole('button', { name: 'Save policies' }).click();
	// The new folder may already be scanned by itself, so its counts are not
	// checked here.
	await expect(page.getByRole('heading', { name: 'Journey music' })).toBeVisible();

	await page.getByRole('button', { name: 'Scan for changes' }).click();
	await expect(page.getByText('2 indexed · 2 on disk')).toBeVisible();

	await page.goto('/library/albums');
	await page.getByRole('link', { name: 'Open OK Computer' }).click();
	await expect(page.getByRole('button', { name: 'Play Airbag' })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Play Paranoid Android' })).toBeVisible();

	await page.getByRole('button', { name: 'Play', exact: true }).click();
	await expect
		.poll(() => page.evaluate(() => (window as unknown as { __played: string[] }).__played))
		.toContainEqual(expect.stringContaining('/api/v3/stream/'));
});
