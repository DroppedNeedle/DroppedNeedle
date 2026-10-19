import type { BeforeNavigate } from '@sveltejs/kit';
import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import type { JellyfinSettings } from '$lib/queries/settings/JellyfinSettingsQueries.svelte';
import { apiError, fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { SECRET_MASK } from './secretMask';
import SettingsSectionTestHarness from './SettingsSectionTestHarness.svelte';

const nav = vi.hoisted(() => ({
	guard: null as ((navigation: BeforeNavigate) => void) | null,
	goto: vi.fn()
}));
vi.mock('$app/navigation', () => ({
	beforeNavigate: (fn: (navigation: BeforeNavigate) => void) => {
		nav.guard = fn;
	},
	goto: nav.goto
}));

const ROUTE = 'GET /api/v3/settings/jellyfin';
const SAVE = 'PUT /api/v3/settings/jellyfin';

const stored: JellyfinSettings = {
	jellyfin_url: 'http://jellyfin:8096',
	api_key: SECRET_MASK,
	user_id: 'u1',
	enabled: true,
	login_enabled: false
};

const urlField = () => page.getByRole('textbox', { name: 'Server URL' });
const saveButton = () => page.getByRole('button', { name: 'Save' });

function linkTo(path: string) {
	const cancel = vi.fn();
	const navigation = {
		type: 'link',
		willUnload: false,
		delta: undefined,
		to: { url: new URL(path, window.location.origin) },
		cancel
	} as unknown as BeforeNavigate;
	return { navigation, cancel };
}

describe('settings section', () => {
	it('keeps unsaved edits through a refetch, sends an untouched secret as its mask, then resets on save', async () => {
		const api = fakeApi({ [ROUTE]: stored, [SAVE]: ({ body }) => body as JellyfinSettings });
		const view = await renderWithQuery(SettingsSectionTestHarness, {});

		await expect.element(urlField()).toHaveValue('http://jellyfin:8096');
		await expect.element(saveButton()).toBeDisabled();

		await urlField().fill('http://media.lan:8096');
		await expect.element(saveButton()).toBeEnabled();

		api.use({ [ROUTE]: { ...stored, jellyfin_url: 'http://changed-elsewhere:8096' } });
		await view.client.invalidateQueries();
		await vi.waitFor(() => expect(api.calls(ROUTE)).toHaveLength(2));
		await expect.element(urlField()).toHaveValue('http://media.lan:8096');

		await saveButton().click();
		await expect.element(page.getByText('Saved.', { exact: true })).toBeVisible();
		expect(api.calls(SAVE)[0].body).toEqual({ ...stored, jellyfin_url: 'http://media.lan:8096' });
		await expect.element(saveButton()).toBeDisabled();
	});

	it("shows a refused save's message and keeps the edit", async () => {
		fakeApi({
			[ROUTE]: stored,
			[SAVE]: apiError(400, 'INVALID_INPUT', 'The URL must start with http or https.')
		});
		await renderWithQuery(SettingsSectionTestHarness, {});

		await urlField().fill('jellyfin.lan');
		await saveButton().click();

		await expect.element(page.getByText('The URL must start with http or https.')).toBeVisible();
		await expect.element(urlField()).toHaveValue('jellyfin.lan');
		await expect.element(saveButton()).toBeEnabled();
	});

	it('asks before unsaved edits are lost to a tab switch or a link, and lets a clean form go', async () => {
		fakeApi({ [ROUTE]: stored });
		await renderWithQuery(SettingsSectionTestHarness, {});
		await expect.element(urlField()).toHaveValue('http://jellyfin:8096');

		const clean = linkTo('/library');
		nav.guard?.(clean.navigation);
		expect(clean.cancel).not.toHaveBeenCalled();

		await urlField().fill('http://media.lan:8096');
		await page.getByRole('button', { name: 'Other tab' }).click();
		await page.getByRole('dialog').getByRole('button', { name: 'Keep editing' }).click();
		await expect.element(page.getByText('On the tab')).toBeVisible();
		await expect.element(urlField()).toHaveValue('http://media.lan:8096');

		const dirty = linkTo('/library');
		nav.guard?.(dirty.navigation);
		expect(dirty.cancel).toHaveBeenCalled();
		await expect.element(page.getByText('Discard unsaved changes?')).toBeVisible();
		expect(nav.goto).not.toHaveBeenCalled();
		await page.getByRole('dialog').getByRole('button', { name: 'Discard' }).click();
		await vi.waitFor(() => expect(nav.goto).toHaveBeenCalledWith(dirty.navigation.to?.url));
		await expect.element(urlField()).toHaveValue('http://jellyfin:8096');
	});
});
