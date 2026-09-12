import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { ProfileServiceConnection } from '$lib/queries/profile/types';

const h = vi.hoisted(() => ({
	connections: [] as Array<{ service: string; enabled: boolean; username: string }>,
	isPending: false,
	connectNavidrome: vi.fn().mockResolvedValue({}),
	connectJellyfin: vi.fn().mockResolvedValue({}),
	plexPin: vi.fn().mockResolvedValue({ pin_id: 7, auth_url: 'https://app.plex.tv/auth#?code=x' }),
	plexPoll: vi.fn().mockResolvedValue({ completed: false, username: '' }),
	disconnect: vi.fn().mockResolvedValue({})
}));

vi.mock('$lib/queries/connections/ConnectionsQuery.svelte', () => ({
	getConnectionsQuery: () => ({
		get data() {
			return { connections: h.connections };
		},
		get isPending() {
			return h.isPending;
		}
	})
}));

vi.mock('$lib/queries/connections/ConnectionsMutations.svelte', () => ({
	createConnectNavidromeMutation: () => ({ mutateAsync: h.connectNavidrome, isPending: false }),
	createConnectJellyfinMutation: () => ({ mutateAsync: h.connectJellyfin, isPending: false }),
	createPlexLinkPinMutation: () => ({ mutateAsync: h.plexPin, isPending: false }),
	createPlexLinkPollMutation: () => ({ mutateAsync: h.plexPoll, isPending: false }),
	createDisconnectMutation: () => ({ mutateAsync: h.disconnect, isPending: false })
}));

import MediaServerAccountsCard from './MediaServerAccountsCard.svelte';

const ALL_SERVICES: ProfileServiceConnection[] = [
	{ name: 'Jellyfin', enabled: true, username: '', url: 'http://jf.local' },
	{ name: 'Navidrome', enabled: true, username: 'admin', url: 'http://nd.local' },
	{ name: 'Plex', enabled: true, username: '', url: 'http://plex.local' }
];

beforeEach(() => {
	h.connections = [];
	h.isPending = false;
	vi.clearAllMocks();
});

describe('MediaServerAccountsCard.svelte', () => {
	it('renders a row per admin-enabled server with the shared-account caption', async () => {
		await render(MediaServerAccountsCard, { services: ALL_SERVICES });
		await expect
			.element(page.getByRole('heading', { name: 'Media Server Accounts', level: 2 }))
			.toBeInTheDocument();
		await expect.element(page.getByText('Navidrome', { exact: true })).toBeInTheDocument();
		await expect.element(page.getByText('Jellyfin', { exact: true })).toBeInTheDocument();
		await expect.element(page.getByText('Plex', { exact: true })).toBeInTheDocument();
		expect(page.getByText('Plays use the shared account').elements().length).toBe(3);
	});

	it('renders nothing when no media server is enabled', async () => {
		const { container } = await render(MediaServerAccountsCard, {
			services: ALL_SERVICES.map((s) => ({ ...s, enabled: false }))
		});
		expect(container.querySelector('section')).toBeNull();
	});

	it('hides servers the admin has not enabled', async () => {
		await render(MediaServerAccountsCard, {
			services: ALL_SERVICES.filter((s) => s.name === 'Navidrome')
		});
		await expect.element(page.getByText('Navidrome', { exact: true })).toBeInTheDocument();
		expect(page.getByText('Plex', { exact: true }).elements().length).toBe(0);
	});

	it('links a Navidrome account through the credentials form', async () => {
		await render(MediaServerAccountsCard, {
			services: ALL_SERVICES.filter((s) => s.name === 'Navidrome')
		});
		await page.getByRole('button', { name: 'Connect' }).click();
		await page.getByPlaceholder('Navidrome username').fill('alice');
		await page.getByPlaceholder('Password').fill('pw-1');
		await page.getByRole('button', { name: 'Link account' }).click();
		expect(h.connectNavidrome).toHaveBeenCalledWith({ username: 'alice', password: 'pw-1' });
	});

	it('shows the linked identity and disconnects', async () => {
		h.connections = [{ service: 'jellyfin', enabled: true, username: 'alice_jf' }];
		await render(MediaServerAccountsCard, {
			services: ALL_SERVICES.filter((s) => s.name === 'Jellyfin')
		});
		await expect.element(page.getByText('Plays count as @alice_jf')).toBeInTheDocument();
		await page.getByRole('button', { name: 'Disconnect' }).click();
		expect(h.disconnect).toHaveBeenCalledWith('jellyfin');
	});

	it('starts the Plex pin flow and shows the waiting state', async () => {
		const openSpy = vi.spyOn(window, 'open').mockReturnValue(null);
		await render(MediaServerAccountsCard, {
			services: ALL_SERVICES.filter((s) => s.name === 'Plex')
		});
		await page.getByRole('button', { name: 'Connect' }).click();
		expect(h.plexPin).toHaveBeenCalled();
		expect(openSpy).toHaveBeenCalledWith(
			'https://app.plex.tv/auth#?code=x',
			'_blank',
			'popup=yes,noopener,noreferrer'
		);
		await expect.element(page.getByText('Waiting for Plex…')).toBeInTheDocument();
		openSpy.mockRestore();
	});

	it('surfaces a link error inline', async () => {
		h.connectJellyfin.mockRejectedValueOnce(new Error('boom'));
		await render(MediaServerAccountsCard, {
			services: ALL_SERVICES.filter((s) => s.name === 'Jellyfin')
		});
		await page.getByRole('button', { name: 'Connect' }).click();
		await page.getByPlaceholder('Jellyfin username').fill('alice');
		await page.getByPlaceholder('Password').fill('bad');
		await page.getByRole('button', { name: 'Link account' }).click();
		await expect.element(page.getByText('Could not sign in to Jellyfin.')).toBeInTheDocument();
	});
});
