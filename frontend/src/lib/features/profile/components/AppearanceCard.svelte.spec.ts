// The custom CSS editor: a save applies the sheet and Undo puts the last one
// back; a refused part is marked on its own line and cannot be saved; a
// saved version comes back into the editor without being saved again.
import { flushSync } from 'svelte';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { components } from '$lib/api/v3/openapi';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { MAX_CUSTOM_CSS } from '$lib/stores/customCss.svelte';
import { fakeApi, type FakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { clearUserSessionState } from '$lib/utils/userSessionCleanup';
import AppearanceCardTestHarness from './AppearanceCardTestHarness.svelte';

type MyAppearance = components['schemas']['MyAppearance'];
type AppearanceUpdate = components['schemas']['AppearanceUpdate'];
type CssSave = components['schemas']['CssSave'];

let stored: MyAppearance;
let saves: CssSave[];
let api: FakeApi;

function token(name: string): string {
	return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function editor() {
	return page.getByRole('textbox', { name: 'CSS rules' });
}

/** The text of each line the editor marks as refused. */
function markedLines(): string[] {
	const root = document.querySelector('.code-host')?.shadowRoot;
	return [...(root?.querySelectorAll('.cm-line') ?? [])]
		.filter((line) => line.querySelector('.cm-lintRange-error'))
		.map((line) => line.textContent ?? '');
}

beforeEach(async () => {
	localStorage.clear();
	stored = {
		theme: 'dark',
		custom_css: { text: '', enabled: true, saved_at: null },
		site: { css: '', allow_user_css: true },
		stored: true
	};
	saves = [{ id: 1, css: ':root { --dn-spec-old: 3px; }', saved_at: Date.now() - 60_000 }];
	api = fakeApi({
		'GET /api/v3/me/appearance': () => stored,
		'PUT /api/v3/me/appearance': ({ body }) => {
			const update = body as AppearanceUpdate;
			stored = {
				...stored,
				custom_css: {
					...stored.custom_css,
					text: update.custom_css?.text ?? stored.custom_css.text,
					saved_at: Date.now()
				}
			};
			return stored;
		},
		'GET /api/v3/me/appearance/css-saves': () => saves
	});
	authStore.setUser({
		id: 'user-css',
		display_name: 'user-css',
		role: 'user',
		email: null,
		avatar_url: null,
		username: null,
		username_display: null,
		providers: []
	});
	flushSync();
	await renderWithQuery(AppearanceCardTestHarness, {}, { client: queryClient });
	await expect.element(editor()).toBeVisible();
});

afterEach(async () => {
	await clearUserSessionState();
	flushSync();
});

describe('custom CSS editor', () => {
	it('applies a saved sheet and Undo puts the previous one back', async () => {
		await editor().fill(':root { --dn-spec-e: 2px; }');
		await expect.element(page.getByText('Previewing unsaved changes.')).toBeVisible();
		expect(token('--dn-spec-e')).toBe('2px');

		await page.getByRole('button', { name: 'Save', exact: true }).click();
		await expect.element(page.getByText('CSS saved.')).toBeVisible();
		expect(api.calls('PUT /api/v3/me/appearance').map((call) => call.body)).toEqual([
			{ custom_css: { text: ':root { --dn-spec-e: 2px; }' } }
		]);
		await expect.poll(() => token('--dn-spec-e')).toBe('2px');

		await page.getByRole('button', { name: 'Undo' }).click();
		await expect
			.poll(() => api.calls('PUT /api/v3/me/appearance').at(-1)?.body)
			.toEqual({ custom_css: { text: '' } });
		await expect.poll(() => token('--dn-spec-e')).toBe('');
		await expect.element(page.getByText(`0 / ${MAX_CUSTOM_CSS.toLocaleString()}`)).toBeVisible();
	});

	it('marks a refused part on its own line and does not save or apply it', async () => {
		await editor().fill(':root { --dn-spec-r: 1px; }\nb { background: url(x.png); }');
		await expect.element(page.getByText('Not allowed: url().')).toBeVisible();
		await expect.poll(markedLines).toEqual(['b { background: url(x.png); }']);
		await expect.element(page.getByRole('button', { name: 'Save', exact: true })).toBeDisabled();
		expect(token('--dn-spec-r')).toBe('');
		expect(api.calls('PUT /api/v3/me/appearance')).toEqual([]);
	});

	it('restores a saved version into the editor without saving it', async () => {
		await page.getByRole('tab', { name: 'History' }).click();
		await page.getByRole('button', { name: /^Restore the save from/ }).click();

		await expect.element(editor()).toHaveTextContent(':root { --dn-spec-old: 3px; }');
		await expect.poll(() => token('--dn-spec-old')).toBe('3px');
		expect(api.calls('PUT /api/v3/me/appearance')).toEqual([]);
	});
});
