import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	rosterError: false,
	settings: {
		subsonic_enabled: true,
		jellyfin_enabled: true,
		transcoding_enabled: true,
		transcode_default_format: 'mp3',
		transcode_max_bitrate_kbps: 320,
		advertise_server_name: 'DroppedNeedle',
		advertise_server_version: '10.10.6',
		discover_mode: 'local-only'
	},
	roster: {
		items: [
			{
				id: 'ap-1',
				user_id: 'user-alice',
				owner_username: 'alice',
				owner_display_name: 'Alice',
				name: 'Symfonium (phone)',
				created_at: '2026-06-01T00:00:00Z',
				last_used_at: null,
				last_client: null
			}
		],
		active_count: 1
	},
	saveMutate: vi.fn().mockResolvedValue({}),
	adminRevokeMutate: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('$lib/queries/connect-apps/ConnectAppsQueries.svelte', () => ({
	getConnectAppsSettingsQuery: () => ({ data: h.settings, isLoading: false, isError: false }),
	getAdminAppPasswordsQuery: () => ({
		data: h.roster,
		isLoading: false,
		isError: h.rosterError,
		refetch: vi.fn()
	})
}));

vi.mock('$lib/queries/connect-apps/ConnectAppsMutations.svelte', () => ({
	saveConnectAppsSettings: () => ({ mutateAsync: h.saveMutate, isPending: false }),
	adminRevokeAppPassword: () => ({ mutateAsync: h.adminRevokeMutate, isPending: false })
}));

vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));

import SettingsConnectApps from './SettingsConnectApps.svelte';

beforeEach(() => {
	h.rosterError = false;
	h.settings = {
		subsonic_enabled: true,
		jellyfin_enabled: true,
		transcoding_enabled: true,
		transcode_default_format: 'mp3',
		transcode_max_bitrate_kbps: 320,
		advertise_server_name: 'DroppedNeedle',
		advertise_server_version: '10.10.6',
		discover_mode: 'local-only'
	};
	h.roster = {
		items: [
			{
				id: 'ap-1',
				user_id: 'user-alice',
				owner_username: 'alice',
				owner_display_name: 'Alice',
				name: 'Symfonium (phone)',
				created_at: '2026-06-01T00:00:00Z',
				last_used_at: null,
				last_client: null
			}
		],
		active_count: 1
	};
	vi.clearAllMocks();
});

describe('SettingsConnectApps.svelte (admin)', () => {
	it('renders the server-setup header, protocol toggles, and the Profile pointer', async () => {
		await render(SettingsConnectApps);
		await expect
			.element(page.getByRole('heading', { name: 'Connect Apps', level: 2 }))
			.toBeInTheDocument();
		await expect.element(page.getByLabelText('Enable OpenSubsonic API')).toBeInTheDocument();
		await expect.element(page.getByLabelText('Enable Jellyfin API')).toBeInTheDocument();
		await expect.element(page.getByText(/Profile → Connect Apps/)).toBeInTheDocument();
	});

	it("lists every user's app-password with owner, count, and no secret", async () => {
		await render(SettingsConnectApps);
		await expect.element(page.getByText('Alice', { exact: true })).toBeInTheDocument();
		await expect.element(page.getByText('@alice')).toBeInTheDocument();
		await expect.element(page.getByText('Symfonium (phone)')).toBeInTheDocument();
		await expect.element(page.getByText('1 active')).toBeInTheDocument();
	});

	it('confirms before an admin revoke, then revokes with the owner id on confirm', async () => {
		await render(SettingsConnectApps);
		// the row action only opens the dialog
		await page.getByRole('button', { name: 'Revoke Symfonium (phone) for alice' }).click();
		await expect
			.element(page.getByText(/Revoke "Symfonium \(phone\)" for Alice\?/))
			.toBeInTheDocument();
		expect(h.adminRevokeMutate).not.toHaveBeenCalled();
		// confirm passes BOTH id and owner user_id (the self-revoke invalidation relies on it)
		await page.getByRole('button', { name: 'Revoke', exact: true }).click();
		expect(h.adminRevokeMutate).toHaveBeenCalledWith({ id: 'ap-1', userId: 'user-alice' });
	});

	it('shows an error with a working retry when the roster fails to load', async () => {
		h.rosterError = true;
		await render(SettingsConnectApps);
		await expect
			.element(page.getByText('Could not load the app-password list.'))
			.toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Try again' })).toBeInTheDocument();
	});

	it('shows an empty state when no app-passwords exist', async () => {
		h.roster = { items: [], active_count: 0 };
		await render(SettingsConnectApps);
		await expect
			.element(page.getByText('No app-passwords have been created yet.'))
			.toBeInTheDocument();
	});

	it('saves settings via the mutation', async () => {
		await render(SettingsConnectApps);
		await page.getByRole('button', { name: 'Save' }).click();
		expect(h.saveMutate).toHaveBeenCalledOnce();
	});
});
