import { readFile } from 'node:fs/promises';
import { test, expect, setUpAdmin } from './fixtures';

// The script mounts Radiohead's "Airbag" and "Paranoid Android" at /music.
// One artist cannot fill a year, so the journey adds plays of four more by
// name, through the scrobble route the web player sends finished plays to.
const OTHERS = ['Portishead', 'Massive Attack', 'Bjork', 'Tricky'];

test('a year with enough plays saves its story image as a PNG', async ({ page, baseURL }) => {
	await setUpAdmin(page);

	await page.goto('/settings?tab=library');
	await page.getByRole('button', { name: 'Add root' }).click();
	const addRoot = page.getByRole('dialog');
	await addRoot.getByLabel('Name').fill('Journey music');
	await addRoot.getByLabel('Folder path').fill('/music');
	await addRoot.getByRole('button', { name: 'Add root', exact: true }).click();
	await page.getByRole('button', { name: 'Preview and save settings' }).click();
	await page.getByRole('dialog').getByRole('button', { name: 'Save policies' }).click();
	await expect(page.getByRole('heading', { name: 'Journey music' })).toBeVisible();
	await page.getByRole('button', { name: 'Scan for changes' }).click();
	await expect(page.getByText('2 indexed · 2 on disk')).toBeVisible();

	const listed = await page.request.get('/api/v3/library/tracks');
	expect(listed.ok()).toBe(true);
	const tracks = ((await listed.json()) as { items: { id: string; title: string }[] }).items;
	expect(tracks).toHaveLength(2);

	// 20 plays of the library's album, then 4 each of four artists by name,
	// four minutes apart and ending now.
	const plays = [
		...Array.from({ length: 20 }, (_, i) => ({
			track_id: tracks[i % 2].id,
			track_name: tracks[i % 2].title,
			artist_name: 'Radiohead'
		})),
		...OTHERS.flatMap((artist) =>
			Array.from({ length: 4 }, (_, i) => ({ track_name: `Song ${i + 1}`, artist_name: artist }))
		)
	];
	const now = Math.floor(Date.now() / 1000);
	const origin = new URL(baseURL ?? '').origin;
	for (const [i, play] of plays.entries()) {
		const sent = await page.request.post('/api/v3/scrobble/submit', {
			headers: { Origin: origin },
			data: { ...play, duration_ms: 240_000, timestamp: now - i * 240 }
		});
		expect(sent.ok()).toBe(true);
	}

	await page.goto('/your-year');
	await page.getByRole('button', { name: 'Save image' }).first().click();
	const sheet = page.getByRole('dialog', { name: 'Save image' });
	await expect(sheet.getByRole('img', { name: 'Preview of the story image' })).toBeVisible();

	const downloaded = page.waitForEvent('download');
	await sheet.getByRole('button', { name: 'Save', exact: true }).click();
	const download = await downloaded;
	const year = new Date().getFullYear();
	expect(download.suggestedFilename()).toBe(`droppedneedle-${year}-story.png`);

	// A PNG's signature, then its IHDR chunk: width and height, big-endian.
	const png = await readFile(await download.path());
	expect([...png.subarray(0, 8)]).toEqual([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
	expect(png.subarray(12, 16).toString('ascii')).toBe('IHDR');
	expect([png.readUInt32BE(16), png.readUInt32BE(20)]).toEqual([1080, 1920]);
});
