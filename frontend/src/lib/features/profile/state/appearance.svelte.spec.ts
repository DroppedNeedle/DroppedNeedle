// A person's theme and custom CSS live on their account. Their sheet applies
// while they are signed in, leaves at sign-out and never reaches the next
// person on the browser. A sheet that could fetch anything is refused and
// not applied. What a browser kept before moves to the account once, and a
// refused sheet stays behind while the theme still moves.
import { flushSync } from 'svelte';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { components } from '$lib/api/v3/openapi';
import { AppearanceQueryKeyFactory } from '$lib/queries/featureKeys';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore, type AuthUser } from '$lib/stores/authStore.svelte';
import { MAX_CUSTOM_CSS } from '$lib/stores/customCss.svelte';
import { themeStore } from '$lib/stores/theme.svelte';
import { toastStore } from '$lib/stores/toast';
import AppearanceTestHarness from './AppearanceTestHarness.svelte';
import { apiError, fakeApi, type FakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { clearUserSessionState } from '$lib/utils/userSessionCleanup';

type MyAppearance = components['schemas']['MyAppearance'];
type AppearanceUpdate = components['schemas']['AppearanceUpdate'];

function user(id: string): AuthUser {
	return {
		id,
		display_name: id,
		role: 'user',
		email: null,
		avatar_url: null,
		username: null,
		username_display: null,
		providers: []
	};
}

function token(name: string): string {
	return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

// The owner tag the browser keeps for an account, made here with Web Crypto.
async function tagOf(id: string): Promise<string> {
	const digest = await crypto.subtle.digest(
		'SHA-256',
		new TextEncoder().encode(`droppedneedle theme owner:${id}`)
	);
	return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
}

function signIn(id: string): void {
	authStore.setUser(user(id));
	flushSync();
}

let site = { css: '', allow_user_css: true };
let accounts: Map<string, MyAppearance>;
let api: FakeApi;

function account(id: string): MyAppearance {
	return (
		accounts.get(id) ?? {
			theme: null,
			custom_css: { text: '', enabled: true, saved_at: null },
			site,
			stored: false
		}
	);
}

function write(id: string, update: AppearanceUpdate): MyAppearance {
	const now = account(id);
	const next: MyAppearance = {
		theme: update.theme ?? now.theme,
		custom_css: {
			text: update.custom_css?.text ?? now.custom_css.text,
			enabled: update.custom_css?.enabled ?? now.custom_css.enabled,
			saved_at: 1
		},
		site,
		stored: true
	};
	accounts.set(id, next);
	return next;
}

function serve(): FakeApi {
	return fakeApi({
		'GET /api/v3/me/appearance': () => ({ ...account(authStore.user?.id ?? ''), site }),
		'PUT /api/v3/me/appearance': ({ body }) => write(authStore.user?.id ?? '', body as never),
		'POST /api/v3/me/appearance/import': ({ body }) => {
			const id = authStore.user?.id ?? '';
			if (accounts.has(id)) return { ...account(id), imported: false };
			return { ...write(id, body as AppearanceUpdate), imported: true };
		}
	});
}

async function mount(): Promise<void> {
	await renderWithQuery(AppearanceTestHarness, {}, { client: queryClient });
}

function editor() {
	return page.getByRole('textbox', { name: 'CSS rules' });
}

async function save(css: string): Promise<void> {
	await editor().fill(css);
	await page.getByRole('button', { name: 'Save', exact: true }).click();
}

beforeEach(() => {
	localStorage.clear();
	site = { css: '', allow_user_css: true };
	accounts = new Map();
	api = serve();
});

afterEach(async () => {
	await clearUserSessionState();
	flushSync();
});

describe('account appearance', () => {
	it("applies one user's sheet and takes it off at sign-out", async () => {
		signIn('user-a');
		await mount();

		await save(':root { --dn-spec-a: 1px; }');
		await expect.poll(() => token('--dn-spec-a')).toBe('1px');
		expect(api.calls('PUT /api/v3/me/appearance')[0]?.body).toEqual({
			custom_css: { text: ':root { --dn-spec-a: 1px; }' }
		});

		// A clean-up that keeps the signed-in user leaves the sheet on.
		await clearUserSessionState({ clearAuth: false, clearLastUserId: false });
		flushSync();
		expect(token('--dn-spec-a')).toBe('1px');

		await clearUserSessionState();
		flushSync();
		expect(token('--dn-spec-a')).toBe('');

		signIn('user-b');
		await expect.element(page.getByText(`0 / ${MAX_CUSTOM_CSS.toLocaleString()}`)).toBeVisible();
		expect(token('--dn-spec-a')).toBe('');

		signIn('user-a');
		await expect.poll(() => token('--dn-spec-a')).toBe('1px');
	});

	it('refuses a sheet that could fetch and applies none of it', async () => {
		signIn('user-a');
		await mount();

		await editor().fill(
			':root { --dn-spec-b: 1px; } body { background: url(https://x.test/a.png); }'
		);
		await expect.element(page.getByText('Not allowed: url().')).toBeVisible();
		await editor().fill(':root { --dn-spec-b: 1px; --x: u\\72l(https://x.test/b.png); }');
		await expect.element(page.getByText(/Not allowed: backslashes/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Save', exact: true })).toBeDisabled();
		expect(token('--dn-spec-b')).toBe('');
		expect(api.calls('PUT /api/v3/me/appearance')).toEqual([]);
	});

	it('does not apply a refused sheet the account holds', async () => {
		write('user-c', {
			custom_css: {
				text: ':root { --dn-spec-c: 1px; } body { background: image-set("https://x.test/c.png" 1x); }'
			}
		});
		signIn('user-c');
		await mount();

		await expect.element(page.getByText('Not allowed: image-set().')).toBeVisible();
		expect(token('--dn-spec-c')).toBe('');
	});

	it('saves a chosen theme to the account and shows the account’s', async () => {
		write('user-t', { theme: 'light' });
		signIn('user-t');
		await mount();

		await expect.poll(() => themeStore.preference).toBe('light');
		expect(localStorage.getItem('droppedneedle_theme')).toBe('light');

		await page.getByText('Midnight', { exact: true }).click();
		expect(themeStore.preference).toBe('midnight');
		await expect
			.poll(() => api.calls('PUT /api/v3/me/appearance').at(-1)?.body)
			.toEqual({
				theme: 'midnight'
			});
	});

	it("moves this browser's theme and CSS to the account once", async () => {
		localStorage.setItem('droppedneedle_custom_css:user-d', ':root { --dn-spec-d: 1px; }');
		localStorage.setItem('droppedneedle_custom_css:user-d2', ':root { --dn-spec-x: 1px; }');
		localStorage.setItem('droppedneedle_theme', 'light');
		signIn('user-d');
		await mount();

		await expect.poll(() => token('--dn-spec-d')).toBe('1px');
		expect(api.calls('POST /api/v3/me/appearance/import').map((call) => call.body)).toEqual([
			{ theme: 'light', custom_css: { text: ':root { --dn-spec-d: 1px; }' } }
		]);
		expect(localStorage.getItem('droppedneedle_custom_css:user-d')).toBeNull();
		expect(localStorage.getItem('droppedneedle_custom_css:user-d2')).not.toBeNull();
		expect(localStorage.getItem('droppedneedle_theme')).toBe('light');
		await expect
			.poll(() => localStorage.getItem('droppedneedle_theme_owner'))
			.toBe(await tagOf('user-d'));
	});

	it('keeps a refused sheet in this browser and still moves the theme', async () => {
		api.use({
			'POST /api/v3/me/appearance/import': ({ body }) => {
				const update = body as AppearanceUpdate;
				if (update.custom_css) {
					return apiError(400, 'CUSTOM_CSS_REFUSED', 'Refused', {
						problems: [{ what: 'function:url', line: 1 }]
					});
				}
				return { ...write('user-e', update), imported: true };
			}
		});
		localStorage.setItem('droppedneedle_custom_css:user-e', 'a { b: url(x); }');
		localStorage.setItem('droppedneedle_theme', 'midnight');
		signIn('user-e');
		await mount();

		await expect.poll(() => api.calls('POST /api/v3/me/appearance/import').length).toBe(2);
		expect(api.calls('POST /api/v3/me/appearance/import')[1]?.body).toEqual({
			theme: 'midnight'
		});
		await expect.poll(() => accounts.get('user-e')?.theme).toBe('midnight');
		expect(localStorage.getItem('droppedneedle_custom_css:user-e')).toBe('a { b: url(x); }');
	});

	it("does not move another account's theme to this one or show it", async () => {
		localStorage.setItem('droppedneedle_theme', 'light');
		localStorage.setItem('droppedneedle_theme_owner', await tagOf('user-f0'));
		themeStore.set('light');
		signIn('user-f');
		await mount();

		await expect.poll(() => themeStore.preference).toBe('dark');
		expect(document.documentElement.dataset.theme).toBe('dark');

		// The default replaces the copy and is claimed for this account, once the account answers.
		const tagF = await tagOf('user-f');
		await expect.poll(() => localStorage.getItem('droppedneedle_theme_owner')).toBe(tagF);
		expect(localStorage.getItem('droppedneedle_theme')).toBe('dark');
		expect(api.calls('POST /api/v3/me/appearance/import')).toEqual([]);
		expect(api.calls('PUT /api/v3/me/appearance')).toEqual([]);
	});

	it("still moves this browser's theme to the account that owns it", async () => {
		localStorage.setItem('droppedneedle_theme', 'midnight');
		localStorage.setItem('droppedneedle_theme_owner', await tagOf('user-g'));
		signIn('user-g');
		await mount();

		await expect.poll(() => accounts.get('user-g')?.theme).toBe('midnight');
		expect(api.calls('POST /api/v3/me/appearance/import').map((call) => call.body)).toEqual([
			{ theme: 'midnight' }
		]);
		await expect.poll(() => themeStore.preference).toBe('midnight');
		expect(localStorage.getItem('droppedneedle_theme_owner')).toBe(await tagOf('user-g'));
		expect(JSON.stringify({ ...localStorage })).not.toContain('user-g');
	});

	it('gives an unclaimed theme to the first account that signs in, not the next', async () => {
		write('user-h', { custom_css: { text: ':root { --dn-spec-h: 1px; }' } });
		localStorage.setItem('droppedneedle_theme', 'light');
		signIn('user-h');
		await mount();
		await expect.poll(() => token('--dn-spec-h')).toBe('1px');
		await expect
			.poll(() => localStorage.getItem('droppedneedle_theme_owner'))
			.toBe(await tagOf('user-h'));

		signIn('user-i');
		await expect.poll(() => themeStore.preference).toBe('dark');
		expect(api.calls('POST /api/v3/me/appearance/import')).toEqual([]);
	});

	it("shows the default at once for another account's copy, then claims that copy", async () => {
		themeStore.set('light');
		localStorage.setItem('droppedneedle_theme_owner', await tagOf('user-l0'));
		const tagL = await tagOf('user-l');

		// The account's answer is held back, so the check before it is what shows.
		let release!: () => void;
		const answered = new Promise<void>((resolve) => {
			release = () => resolve();
		});
		api.use({
			'GET /api/v3/me/appearance': async () => {
				await answered;
				return account('user-l');
			}
		});
		await mount();
		signIn('user-l');

		await expect.poll(() => themeStore.preference).toBe('dark');
		expect(document.documentElement.dataset.theme).toBe('dark');
		expect(localStorage.getItem('droppedneedle_theme')).toBe('light');

		release();
		await expect.poll(() => localStorage.getItem('droppedneedle_theme_owner')).toBe(tagL);
		expect(localStorage.getItem('droppedneedle_theme')).toBe('dark');
		expect(api.calls('PUT /api/v3/me/appearance')).toEqual([]);
		expect(api.calls('POST /api/v3/me/appearance/import')).toEqual([]);
	});

	it('sends theme saves one at a time, and the newest choice is the one stored', async () => {
		write('user-m', { theme: 'light' });
		signIn('user-m');
		await mount();
		await expect.poll(() => themeStore.preference).toBe('light');

		// The first save is held open while two more choices are made.
		let release!: () => void;
		const held = new Promise<void>((resolve) => {
			release = () => resolve();
		});
		let sent = 0;
		api.use({
			'PUT /api/v3/me/appearance': async ({ body }) => {
				sent++;
				if (sent === 1) await held;
				return write('user-m', body as AppearanceUpdate);
			}
		});

		const newestTheme = () =>
			queryClient.getQueryData<MyAppearance>(AppearanceQueryKeyFactory.mine('user-m'))?.theme;
		const shown: string[] = [];
		const watch = new MutationObserver(() => {
			shown.push(document.documentElement.dataset.theme ?? '');
		});

		try {
			await page.getByText('Midnight', { exact: true }).click();
			await page.getByText('Light', { exact: true }).click();
			await expect.poll(() => api.calls('PUT /api/v3/me/appearance').length).toBeGreaterThan(0);
			expect(api.calls('PUT /api/v3/me/appearance')).toHaveLength(1);

			// From the newest choice on, every theme the page shows must be that choice.
			watch.observe(document.documentElement, {
				attributes: true,
				attributeFilter: ['data-theme']
			});
			await page.getByText('Dark', { exact: true }).click();
			expect(themeStore.preference).toBe('dark');
			release();
			await expect.poll(() => api.calls('PUT /api/v3/me/appearance').length).toBe(2);
			await expect.poll(newestTheme).toBe('dark');
			expect(shown).toContain('dark');
			expect(shown.every((theme) => theme === 'dark')).toBe(true);
			expect(api.calls('PUT /api/v3/me/appearance').map((call) => call.body)).toEqual([
				{ theme: 'midnight' },
				{ theme: 'dark' }
			]);
		} finally {
			watch.disconnect();
			release();
		}
	});

	it('shows one error for a theme save that fails, and keeps the choice on screen', async () => {
		write('user-n', { theme: 'light' });
		signIn('user-n');
		await mount();
		await expect.poll(() => themeStore.preference).toBe('light');
		api.use({ 'PUT /api/v3/me/appearance': () => apiError(500, 'INTERNAL_ERROR', 'Failed') });

		const errors: string[] = [];
		const stop = toastStore.subscribe((toast) => {
			if (toast?.type === 'error') errors.push(toast.message);
		});
		try {
			await page.getByText('Midnight', { exact: true }).click();
			await expect.poll(() => errors).toEqual(['Theme not saved to your account.']);
			expect(themeStore.preference).toBe('midnight');
			expect(document.documentElement.dataset.theme).toBe('midnight');
		} finally {
			stop();
		}
	});
});
