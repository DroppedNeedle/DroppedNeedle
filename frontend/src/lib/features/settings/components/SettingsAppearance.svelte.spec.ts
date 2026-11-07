// The site appearance tab: a save sends the site CSS, a save the server
// refuses marks the lines the server named, and the switch for people's own
// CSS saves with the section.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { components } from '$lib/api/v3/openapi';
import { queryClient } from '$lib/queries/QueryClient';
import { apiError, fakeApi, type FakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import SettingsAppearance from './SettingsAppearance.svelte';

type Appearance = components['schemas']['Appearance'];

let stored: Appearance;
let refusedLine: number | null;
let api: FakeApi;

function editor() {
	return page.getByRole('textbox', { name: 'Site CSS' });
}

/** The text of each line the editor marks as refused. */
function markedLines(): string[] {
	const root = document.querySelector('.code-host')?.shadowRoot;
	return [...(root?.querySelectorAll('.cm-line') ?? [])]
		.filter((line) => line.querySelector('.cm-lintRange-error'))
		.map((line) => line.textContent ?? '');
}

beforeEach(async () => {
	stored = { site_css: '', allow_user_css: true };
	refusedLine = null;
	api = fakeApi({
		'GET /api/v3/settings/appearance': () => stored,
		'PUT /api/v3/settings/appearance': ({ body }) => {
			if (refusedLine !== null) {
				return apiError(400, 'CUSTOM_CSS_REFUSED', 'Refused', {
					problems: [{ what: 'import', line: refusedLine }]
				});
			}
			stored = body as Appearance;
			return stored;
		}
	});
	await renderWithQuery(SettingsAppearance, {}, { client: queryClient });
	await expect.element(editor()).toBeVisible();
});

afterEach(() => {
	queryClient.clear();
});

describe('site appearance', () => {
	it('saves the site CSS', async () => {
		await editor().fill(':root { --dn-title: red; }');
		await page.getByRole('button', { name: 'Save', exact: true }).click();

		await expect.element(page.getByText('Saved.')).toBeVisible();
		expect(api.calls('PUT /api/v3/settings/appearance').map((call) => call.body)).toEqual([
			{ site_css: ':root { --dn-title: red; }', allow_user_css: true }
		]);
	});

	it('marks the lines a refused save names', async () => {
		const save = page.getByRole('button', { name: 'Save', exact: true });
		await editor().fill('a { color: red; }\nb { color: blue; }');

		refusedLine = 2;
		await save.click();
		await expect.element(page.getByText('Not allowed: @import.')).toBeVisible();
		await expect.poll(markedLines).toEqual(['b { color: blue; }']);

		// The same text, answered on another line: the marks follow the answer.
		refusedLine = 1;
		await save.click();
		await expect.poll(markedLines).toEqual(['a { color: red; }']);
		expect(stored.site_css).toBe('');
	});

	it("saves the switch for people's own CSS", async () => {
		await page.getByRole('checkbox', { name: 'Let people add their own CSS' }).click();
		await page.getByRole('button', { name: 'Save', exact: true }).click();

		await expect.element(page.getByText('Saved.')).toBeVisible();
		expect(api.calls('PUT /api/v3/settings/appearance').map((call) => call.body)).toEqual([
			{ site_css: '', allow_user_css: false }
		]);
	});
});
