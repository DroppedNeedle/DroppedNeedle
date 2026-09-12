import { page } from '@vitest/browser/context';
import { beforeEach, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const { loadSettingsTab } = vi.hoisted(() => ({
	loadSettingsTab: vi.fn(() => new Promise<never>(() => {}))
}));

vi.mock('./settingsTabs', () => ({ loadSettingsTab }));

import SettingsTabContent from './SettingsTabContent.svelte';

beforeEach(() => {
	loadSettingsTab.mockReset();
	loadSettingsTab.mockImplementation(() => new Promise<never>(() => {}));
});

it('shows an accessible placeholder while loading only the requested tab', async () => {
	const view = await render(SettingsTabContent, { tab: 'library', isAdmin: false });

	await expect.element(page.getByLabelText('Loading settings')).toBeInTheDocument();
	await expect
		.element(page.getByLabelText('Loading settings'))
		.toHaveAttribute('aria-busy', 'true');
	expect(loadSettingsTab).toHaveBeenCalledOnce();
	expect(loadSettingsTab).toHaveBeenLastCalledWith('library', false);

	await view.rerender({ tab: 'users', isAdmin: true });
	expect(loadSettingsTab).toHaveBeenCalledTimes(2);
	expect(loadSettingsTab).toHaveBeenLastCalledWith('users', true);

	await view.unmount();
});

it('shows a retry action when a settings chunk fails to load', async () => {
	loadSettingsTab.mockRejectedValueOnce(new Error('chunk unavailable'));
	const view = await render(SettingsTabContent, { tab: 'library', isAdmin: false });

	await expect.element(page.getByRole('alert')).toBeVisible();
	const retry = page.getByRole('button', { name: 'Try again' });
	await expect.element(retry).toBeEnabled();

	await retry.click();
	await expect.element(page.getByLabelText('Loading settings')).toBeInTheDocument();
	expect(loadSettingsTab).toHaveBeenCalledTimes(2);

	await view.unmount();
});
