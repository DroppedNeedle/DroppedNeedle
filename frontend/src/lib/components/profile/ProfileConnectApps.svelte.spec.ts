import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	isAdmin: false,
	isError: false,
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
	passwords: {
		items: [
			{
				id: 'ap-1',
				name: 'Symfonium (phone)',
				created_at: '2026-06-01T00:00:00Z',
				last_used_at: null,
				last_client: null
			}
		],
		cap: 25,
		active_count: 1
	},
	createMutate: vi.fn().mockResolvedValue({
		secret: 'super-secret-123',
		app_password: {
			id: 'ap-2',
			name: 'Finamp (tablet)',
			created_at: '',
			last_used_at: null,
			last_client: null
		}
	}),
	revokeMutate: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('$lib/queries/connect-apps/ConnectAppsQueries.svelte', () => ({
	getConnectAppsSettingsQuery: () => ({
		data: h.settings,
		isLoading: false,
		isError: h.isError,
		refetch: vi.fn()
	}),
	getAppPasswordsQuery: () => ({
		data: h.passwords,
		isLoading: false,
		isError: h.isError,
		refetch: vi.fn()
	})
}));

vi.mock('$lib/queries/connect-apps/ConnectAppsMutations.svelte', () => ({
	createAppPassword: () => ({ mutateAsync: h.createMutate, isPending: false }),
	revokeAppPassword: () => ({ mutateAsync: h.revokeMutate, isPending: false })
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get isAdmin() {
			return h.isAdmin;
		},
		user: { id: 'user-1', username: 'alice' }
	}
}));

vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));

import ProfileConnectApps from './ProfileConnectApps.svelte';

beforeEach(() => {
	h.isAdmin = false;
	h.isError = false;
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
	h.passwords = {
		items: [
			{
				id: 'ap-1',
				name: 'Symfonium (phone)',
				created_at: '2026-06-01T00:00:00Z',
				last_used_at: null,
				last_client: null
			}
		],
		cap: 25,
		active_count: 1
	};
	vi.clearAllMocks();
});

describe('ProfileConnectApps.svelte', () => {
	it("renders the section, the user's app-passwords, and the cap", async () => {
		await render(ProfileConnectApps);
		await expect
			.element(page.getByRole('heading', { name: 'Connect Apps', level: 2 }))
			.toBeInTheDocument();
		await expect.element(page.getByText('Symfonium (phone)')).toBeInTheDocument();
		await expect.element(page.getByText('1 / 25')).toBeInTheDocument();
	});

	it('renders connection URLs for both protocols', async () => {
		await render(ProfileConnectApps);
		await expect.element(page.getByLabelText('OpenSubsonic server URL')).toBeInTheDocument();
		await expect.element(page.getByLabelText('Jellyfin server URL')).toBeInTheDocument();
		await expect
			.element(page.getByText(/Tested clients: Symfonium, Feishin, Amperfy/))
			.toBeInTheDocument();
	});

	it('reveals the created secret exactly once', async () => {
		await render(ProfileConnectApps);
		await page.getByLabelText('New app-password name').fill('Finamp (tablet)');
		await page.getByRole('button', { name: 'Create' }).click();
		await expect
			.element(page.getByLabelText('App-password secret'))
			.toHaveValue('super-secret-123');
		await expect
			.element(page.getByText(/only time "Finamp \(tablet\)" will be shown/))
			.toBeInTheDocument();
		expect(h.createMutate).toHaveBeenCalledWith('Finamp (tablet)');
	});

	it('disables Create at the cap', async () => {
		h.passwords = { items: [], cap: 25, active_count: 25 };
		await render(ProfileConnectApps);
		await expect.element(page.getByRole('button', { name: 'Create' })).toBeDisabled();
	});

	it('confirms before revoking, then revokes on confirm', async () => {
		await render(ProfileConnectApps);
		// the row action only opens the dialog; nothing is revoked yet
		await page.getByRole('button', { name: 'Revoke Symfonium (phone)' }).click();
		await expect.element(page.getByText(/Revoke "Symfonium \(phone\)"\?/)).toBeInTheDocument();
		expect(h.revokeMutate).not.toHaveBeenCalled();
		// the dialog's confirm button (accessible name exactly "Revoke") does the deed
		await page.getByRole('button', { name: 'Revoke', exact: true }).click();
		expect(h.revokeMutate).toHaveBeenCalledWith('ap-1');
	});

	it('tells a non-admin to wait for the admin when both protocols are off', async () => {
		h.settings = { ...h.settings, subsonic_enabled: false, jellyfin_enabled: false };
		await render(ProfileConnectApps);
		await expect
			.element(page.getByText(/Your admin hasn't turned on streaming yet/))
			.toBeInTheDocument();
		// still lets the user create (pre-provision)
		await expect.element(page.getByRole('button', { name: 'Create' })).toBeInTheDocument();
	});

	it('gives an admin a one-click Enable-in-Settings link when streaming is off', async () => {
		h.isAdmin = true;
		h.settings = { ...h.settings, subsonic_enabled: false, jellyfin_enabled: false };
		await render(ProfileConnectApps);
		await expect
			.element(page.getByRole('link', { name: 'enable it in Settings' }))
			.toHaveAttribute('href', '/settings?tab=connect-apps');
	});

	it('shows an error with a working retry when a query fails', async () => {
		h.isError = true;
		await render(ProfileConnectApps);
		await expect.element(page.getByText("Couldn't load Connect Apps.")).toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Try again' })).toBeInTheDocument();
	});
});
