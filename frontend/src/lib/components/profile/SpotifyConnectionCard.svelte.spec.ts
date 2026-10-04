import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	connections: [] as Array<{ service: string; enabled: boolean; username: string }>,
	connect: vi.fn(),
	disconnect: vi.fn(),
	connectPending: false,
	disconnectPending: false
}));

vi.mock('$lib/queries/connections/ConnectionsQuery.svelte', () => ({
	getConnectionsQuery: () => ({
		get data() {
			return { connections: h.connections };
		}
	})
}));

vi.mock('$lib/queries/connections/ConnectionsMutations.svelte', () => ({
	createConnectSpotifyMutation: () => ({
		mutateAsync: h.connect,
		get isPending() {
			return h.connectPending;
		}
	}),
	createDisconnectMutation: () => ({
		mutateAsync: h.disconnect,
		get isPending() {
			return h.disconnectPending;
		}
	})
}));

vi.mock('$lib/utils/basePath', () => ({
	withBasePath: (path: string) => path
}));

import SpotifyConnectionCard from './SpotifyConnectionCard.svelte';

describe('SpotifyConnectionCard', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		h.connections = [];
		h.connectPending = false;
		h.disconnectPending = false;
		h.connect.mockResolvedValue(undefined);
		h.disconnect.mockResolvedValue(undefined);
	});

	it('shows the linked account with a pointer to playlist import', async () => {
		h.connections = [{ service: 'spotify', enabled: true, username: 'jae' }];

		await render(SpotifyConnectionCard);

		await expect.element(page.getByText('@jae')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Playlists' }))
			.toHaveAttribute('href', '/playlists');
		await expect.element(page.getByRole('button', { name: 'Disconnect' })).toBeVisible();
	});

	it('invites a connection when no Spotify account is linked', async () => {
		await render(SpotifyConnectionCard);

		await expect.element(page.getByText('Not connected')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Connect' })).toBeVisible();
	});

	it('explains a failed connect as a settings problem', async () => {
		h.connect.mockRejectedValue(new Error('no client id'));

		await render(SpotifyConnectionCard);

		await page.getByRole('button', { name: 'Connect' }).click();

		await expect
			.element(page.getByText(/check that spotify is configured in settings/i))
			.toBeVisible();
	});

	it('says plainly that disconnect is not available yet', async () => {
		h.connections = [{ service: 'spotify', enabled: true, username: 'jae' }];
		h.disconnect.mockRejectedValue(new Error('no v3 unlink'));

		await render(SpotifyConnectionCard);

		await page.getByRole('button', { name: 'Disconnect' }).click();

		expect(h.disconnect).toHaveBeenCalledWith('spotify');
		await expect.element(page.getByText('Spotify disconnect is not available yet.')).toBeVisible();
	});
});
