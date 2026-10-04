import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	isPending: false,
	isError: false,
	importUsers: vi.fn(),
	importPending: false,
	imported: vi.fn()
}));

const jellyfinCandidates = [
	{
		provider: 'jellyfin',
		provider_uid: 'jf-1',
		display_name: 'Jae Park',
		email: 'jae@example.com',
		avatar_url: null,
		already_imported: false
	},
	{
		provider: 'jellyfin',
		provider_uid: 'jf-2',
		display_name: 'Robin Lee',
		email: null,
		avatar_url: null,
		already_imported: true
	}
];

const plexCandidates = [
	{
		provider: 'plex',
		provider_uid: 'plex-9',
		display_name: 'Sam Rivera',
		email: 'sam@example.com',
		avatar_url: null,
		already_imported: false
	}
];

vi.mock('$lib/queries/auth/ImportCandidatesQuery.svelte', () => ({
	getImportCandidatesQuery: (provider: () => 'jellyfin' | 'plex') => ({
		get data() {
			return {
				candidates: provider() === 'plex' ? plexCandidates : jellyfinCandidates
			};
		},
		get isPending() {
			return h.isPending;
		},
		get isError() {
			return h.isError;
		}
	})
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'admin-1' } }
}));

vi.mock('$lib/queries/auth/UserImportMutations.svelte', () => ({
	createImportUsersMutation: () => ({
		mutateAsync: h.importUsers,
		get isPending() {
			return h.importPending;
		}
	})
}));

vi.mock('$lib/api/api-utils', () => ({
	getApiUrl: (path: string) => path
}));

import SettingsImportUsers from './SettingsImportUsers.svelte';

describe('SettingsImportUsers', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		h.isPending = false;
		h.isError = false;
		h.importPending = false;
		h.importUsers.mockResolvedValue({ total_imported: 1, linked: [], skipped: [] });
	});

	it('lists Jellyfin accounts first and marks imported ones read-only', async () => {
		await render(SettingsImportUsers, { props: { open: true, onImported: h.imported } });

		await expect.element(page.getByText('Jae Park')).toBeVisible();
		await expect.element(page.getByText('Robin Lee')).toBeVisible();
		await expect.element(page.getByText('Already imported')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Import' })).toBeDisabled();
	});

	it('switches to the Plex directory without carrying the selection over', async () => {
		await render(SettingsImportUsers, { props: { open: true, onImported: h.imported } });

		await page.getByRole('checkbox').first().click();
		await expect.element(page.getByRole('button', { name: 'Import 1' })).toBeVisible();
		await page.getByRole('tab', { name: 'Plex' }).click();

		await expect.element(page.getByText('Sam Rivera')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Import' })).toBeDisabled();
	});

	it('imports the selected accounts and reports the batch result', async () => {
		h.importUsers.mockResolvedValue({
			total_imported: 1,
			linked: [{ id: 'u-2' }],
			skipped: ['jf-stale']
		});
		await render(SettingsImportUsers, { props: { open: true, onImported: h.imported } });

		await page.getByRole('checkbox').first().click();
		await page.getByRole('button', { name: 'Import 1' }).click();

		expect(h.importUsers).toHaveBeenCalledWith({ provider: 'jellyfin', provider_uids: ['jf-1'] });
		await expect
			.element(page.getByText('1 imported, 1 linked to existing, 1 skipped'))
			.toBeVisible();
		expect(h.imported).toHaveBeenCalledOnce();
	});

	it('shows the failure message when the import is rejected', async () => {
		h.importUsers.mockRejectedValue(new Error('directory unreachable'));
		await render(SettingsImportUsers, { props: { open: true, onImported: h.imported } });

		await page.getByRole('checkbox').first().click();
		await page.getByRole('button', { name: 'Import 1' }).click();

		await expect.element(page.getByText('directory unreachable')).toBeVisible();
		expect(h.imported).not.toHaveBeenCalled();
	});
});
