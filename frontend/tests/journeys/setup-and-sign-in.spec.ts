import { test, expect, admin } from './fixtures';

// A fresh server sends the first visitor to set-up. The admin created there
// is signed in straight away, and can sign out and back in with the same
// username and password.
test('a fresh server is set up and its admin signs in again', async ({ page }) => {
	await page.goto('/');
	await expect(page).toHaveURL(/\/setup$/);

	await page.getByLabel('Display Name').fill(admin.displayName);
	await page.getByLabel('Username').fill(admin.username);
	await page.getByLabel('Password', { exact: true }).fill(admin.password);
	await page.getByLabel('Confirm Password').fill(admin.password);
	await page.getByRole('button', { name: 'Create Admin Account' }).click();

	const logOut = page.getByRole('button', { name: 'Log out' });
	await expect(page).toHaveURL(/\/$/);
	await expect(logOut).toBeVisible();

	await logOut.click();
	await expect(page).toHaveURL(/\/login$/);

	await page.getByLabel('Username').fill(admin.username);
	await page.getByLabel('Password', { exact: true }).fill(admin.password);
	await page.getByRole('button', { name: 'Sign in', exact: true }).click();

	await expect(page).toHaveURL(/\/$/);
	await expect(page.getByRole('button', { name: 'Log out' })).toBeVisible();
});

// A network change while the first page loads aborts the download of the
// app's code before the app exists. The page reloads once and shows, rather
// than staying blank.
test('a page whose code fails to download as it opens reloads and shows', async ({ page }) => {
	let documents = 0;
	page.on('request', (request) => {
		if (request.isNavigationRequest() && request.frame() === page.mainFrame()) documents += 1;
	});
	await page.route('**/_app/env.js', (route) =>
		documents < 2 ? route.abort('connectionreset') : route.fallback()
	);

	await page.goto('/');
	await expect(page.getByLabel('Username')).toBeVisible();
	expect(documents).toBe(2);
});
