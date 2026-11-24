import { page } from 'vitest/browser';
import { describe, expect, it, onTestFinished, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { fakeApi } from '$lib/test/api';
import type { YearReview } from '../queries/year.svelte';
import SaveImageButton from './SaveImageButton.svelte';

// Every cover is marked missing, so drawing reads nothing: fakeApi fails
// the test on any request.
function review(state: YearReview['state'] = 'in_progress'): YearReview {
	return {
		year: 2026,
		zone: 'Europe/London',
		state,
		first_day: '2026-01-01',
		last_day: '2026-11-24',
		plays: 400,
		plays_without_length: 0,
		listened_ms: 118 * 3_600_000,
		artists: 40,
		days_played: 120,
		months: [],
		top_albums: [
			{
				id: 'a1',
				name: 'Dummy',
				artist_name: 'Portishead',
				plays: 30,
				art_missing: true,
				cover_available: false,
				release_group_mbid: null
			}
		],
		top_artists: [{ id: 'r1', mbid: null, name: 'Portishead', plays: 60, art_missing: true }],
		top_songs: [],
		new_artists: null
	};
}

/** Lets the page's navigator say whether it can share a file, and records shares. */
function browserShares(can: boolean) {
	const share = vi.fn(() => Promise.resolve());
	const canShare = vi.fn(() => can);
	Object.defineProperty(navigator, 'canShare', { configurable: true, value: canShare });
	Object.defineProperty(navigator, 'share', { configurable: true, value: share });
	onTestFinished(() => {
		Reflect.deleteProperty(navigator, 'canShare');
		Reflect.deleteProperty(navigator, 'share');
	});
	return share;
}

/** Records the file names and addresses of downloads started by a link click. */
function downloads() {
	const saved: { name: string; href: string }[] = [];
	const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
		this: HTMLAnchorElement
	) {
		saved.push({ name: this.download, href: this.href });
	});
	onTestFinished(() => click.mockRestore());
	return saved;
}

async function openSheet() {
	await page.getByRole('button', { name: 'Save image' }).click();
	await expect.element(page.getByRole('dialog', { name: 'Save image' })).toBeVisible();
}

describe('Save image', () => {
	it('is not offered for a year with too few plays', async () => {
		fakeApi({});
		render(SaveImageButton, { review: review('too_few_plays') });

		await expect.element(page.getByRole('button', { name: 'Save image' })).not.toBeInTheDocument();
	});

	it('saves the image of the shape picked as a PNG', async () => {
		fakeApi({});
		browserShares(false);
		const saved = downloads();
		render(SaveImageButton, { review: review() });
		await openSheet();

		await expect
			.element(page.getByRole('img', { name: 'Preview of the story image' }))
			.toBeVisible();
		await page.getByRole('button', { name: 'Save', exact: true }).click();
		await page.getByRole('radio', { name: 'Square' }).click();
		await expect
			.element(page.getByRole('img', { name: 'Preview of the square image' }))
			.toBeVisible();
		await page.getByRole('button', { name: 'Save', exact: true }).click();

		expect(saved.map((file) => file.name)).toEqual([
			'droppedneedle-2026-story.png',
			'droppedneedle-2026-square.png'
		]);
		expect(saved.every((file) => file.href.startsWith('blob:'))).toBe(true);
	});

	it('leaves Share out where the browser cannot share the file', async () => {
		fakeApi({});
		browserShares(false);
		render(SaveImageButton, { review: review() });
		await openSheet();

		await expect
			.element(page.getByRole('img', { name: 'Preview of the story image' }))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Share' })).not.toBeInTheDocument();
	});

	it('shares the PNG where the browser can share the file', async () => {
		fakeApi({});
		const share = browserShares(true);
		render(SaveImageButton, { review: review() });
		await openSheet();
		await page.getByRole('button', { name: 'Share' }).click();

		expect(share).toHaveBeenCalledOnce();
		const [{ files }] = share.mock.calls[0] as unknown as [{ files: File[] }];
		expect(files.map((file) => [file.name, file.type])).toEqual([
			['droppedneedle-2026-story.png', 'image/png']
		]);
	});
});
