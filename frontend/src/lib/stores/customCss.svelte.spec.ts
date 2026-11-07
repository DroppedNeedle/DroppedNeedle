// Custom CSS belongs to the person who saved it: it applies while they are
// signed in, leaves at sign-out, and never reaches the next person on the
// browser. A sheet that could fetch anything (a function off the allowlist,
// or a backslash escape spelling one) is refused at save and is not applied
// from storage either.
import { flushSync } from 'svelte';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import AppearanceCard from '$lib/features/profile/components/AppearanceCard.svelte';
import { authStore, type AuthUser } from '$lib/stores/authStore.svelte';
import { applyCustomCss } from '$lib/stores/customCss.svelte';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { clearUserSessionState } from '$lib/utils/userSessionCleanup';

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

function signIn(id: string): void {
	authStore.setUser(user(id));
	flushSync();
}

async function save(css: string): Promise<void> {
	await page.getByLabelText('CSS rules').fill(css);
	await page.getByRole('button', { name: 'Save' }).click();
}

let stop: (() => void) | null = null;

beforeEach(() => {
	localStorage.clear();
	fakeApi({});
	stop = $effect.root(() => applyCustomCss());
});

afterEach(async () => {
	stop?.();
	await clearUserSessionState();
});

describe('custom CSS', () => {
	it("applies one user's sheet and takes it off at sign-out", async () => {
		signIn('user-a');
		await renderWithQuery(AppearanceCard, {});
		await page.getByText('Custom CSS').click();

		await save(':root { --dn-spec-a: 1px; }');
		await expect.poll(() => token('--dn-spec-a')).toBe('1px');

		await clearUserSessionState();
		flushSync();
		expect(token('--dn-spec-a')).toBe('');

		signIn('user-b');
		await expect.element(page.getByLabelText('CSS rules')).toHaveValue('');
		expect(token('--dn-spec-a')).toBe('');

		signIn('user-a');
		await expect.poll(() => token('--dn-spec-a')).toBe('1px');
	});

	it('refuses a sheet that could fetch and applies none of it', async () => {
		signIn('user-a');
		await renderWithQuery(AppearanceCard, {});
		await page.getByText('Custom CSS').click();

		await save(':root { --dn-spec-b: 1px; } body { background: url(https://x.test/a.png); }');
		await expect.element(page.getByText('Not allowed: url().')).toBeVisible();
		expect(token('--dn-spec-b')).toBe('');

		await save(':root { --dn-spec-b: 1px; --x: u\\72l(https://x.test/b.png); }');
		await expect.element(page.getByText(/Not allowed: backslashes/)).toBeVisible();
		expect(token('--dn-spec-b')).toBe('');
		expect(localStorage.getItem('droppedneedle_custom_css:user-a')).toBeNull();
	});

	it('does not apply a refused sheet already in storage', async () => {
		localStorage.setItem(
			'droppedneedle_custom_css:user-c',
			':root { --dn-spec-c: 1px; } body { background: image-set("https://x.test/c.png" 1x); }'
		);
		signIn('user-c');
		await renderWithQuery(AppearanceCard, {});
		flushSync();

		expect(token('--dn-spec-c')).toBe('');
		await page.getByText('Custom CSS').click();
		await expect.element(page.getByText('Not allowed: image-set().')).toBeVisible();
	});
});
