import { page } from 'vitest/browser';
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
		advertise_server_version: '10.10.6'
	},
	passwords: [
		{
			id: 'ap-1',
			name: 'Symfonium (phone)',
			created_at: '2026-06-01T00:00:00Z',
			last_used_at: null,
			last_client: null
		}
	] as Array<Record<string, unknown>>,
	includePublic: false,
	saveChoices: vi.fn().mockResolvedValue(undefined),
	createMutate: vi.fn().mockResolvedValue({
		id: 'ap-2',
		name: 'Finamp (tablet)',
		created_at: 0,
		secret: 'super-secret-123'
	}),
	revokeMutate: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('$lib/features/profile/queries/connect-apps.svelte', () => ({
	APP_PASSWORD_CAP: 25,
	createAppPasswordMutation: () => ({ mutateAsync: h.createMutate, isPending: false }),
	createRevokeAppPasswordMutation: () => ({ mutateAsync: h.revokeMutate, isPending: false }),
	createSaveMyConnectAppsMutation: () => ({ mutateAsync: h.saveChoices, isPending: false }),
	getMyConnectAppsQuery: () => ({
		data: { include_public_playlists: h.includePublic },
		isLoading: false,
		isError: false,
		refetch: vi.fn()
	}),
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
	h.includePublic = false;
	h.settings = {
		subsonic_enabled: true,
		jellyfin_enabled: true,
		transcoding_enabled: true,
		transcode_default_format: 'mp3',
		transcode_max_bitrate_kbps: 320,
		advertise_server_name: 'DroppedNeedle',
		advertise_server_version: '10.10.6'
	};
	h.passwords = [
		{
			id: 'ap-1',
			name: 'Symfonium (phone)',
			created_at: '2026-06-01T00:00:00Z',
			last_used_at: null,
			last_client: null
		}
	];
	vi.clearAllMocks();
});

describe('ProfileConnectApps.svelte', () => {
	it('reveals the created secret exactly once', async () => {
		await render(ProfileConnectApps);
		// with app-passwords made, the list starts folded
		await page.getByRole('button', { name: /App-passwords/ }).click();
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

	it('confirms before revoking, then revokes on confirm', async () => {
		await render(ProfileConnectApps);
		await page.getByRole('button', { name: /App-passwords/ }).click();
		// the row action only opens the dialog; nothing is revoked yet
		await page.getByRole('button', { name: 'Revoke Symfonium (phone)' }).click();
		await expect.element(page.getByText(/Revoke "Symfonium \(phone\)"\?/)).toBeInTheDocument();
		expect(h.revokeMutate).not.toHaveBeenCalled();
		// the dialog's confirm button (accessible name exactly "Revoke") does the deed
		await page.getByRole('button', { name: 'Revoke', exact: true }).click();
		expect(h.revokeMutate).toHaveBeenCalledWith('ap-1');
	});
});
