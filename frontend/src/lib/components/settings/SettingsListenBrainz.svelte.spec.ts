import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	connection: { username: 'needle', user_token: 'listenbrainz****', enabled: true },
	targets: { scrobble_to_lastfm: false, scrobble_to_listenbrainz: true },
	connPending: false,
	targetsPending: false,
	connError: false,
	saveConn: vi.fn(async (vars: unknown) => vars),
	saveTargets: vi.fn(async (vars: unknown) => vars),
	verify: vi.fn(async () => ({ valid: true, message: 'Connected as needle' })),
	saving: false,
	verifying: false,
	refetchConn: vi.fn()
}));

vi.mock('$lib/queries/listenbrainz/ListenBrainzQuery.svelte', () => ({
	getListenBrainzConnectionQuery: () => ({
		get data() {
			return h.connPending || h.connError ? undefined : h.connection;
		},
		get isPending() {
			return h.connPending;
		},
		get isError() {
			return h.connError;
		},
		refetch: h.refetchConn
	}),
	getScrobbleTargetsQuery: () => ({
		get data() {
			return h.targetsPending ? undefined : h.targets;
		},
		get isPending() {
			return h.targetsPending;
		},
		get isError() {
			return false;
		},
		refetch: vi.fn()
	})
}));

vi.mock('$lib/queries/listenbrainz/ListenBrainzMutations.svelte', () => ({
	createSaveListenBrainzMutation: () => ({
		mutateAsync: h.saveConn,
		get isPending() {
			return h.saving;
		}
	}),
	createSaveScrobbleTargetsMutation: () => ({
		mutateAsync: h.saveTargets,
		get isPending() {
			return h.saving;
		}
	}),
	createVerifyListenBrainzMutation: () => ({
		mutateAsync: h.verify,
		get isPending() {
			return h.verifying;
		}
	})
}));

import SettingsListenBrainz from './SettingsListenBrainz.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.connection = { username: 'needle', user_token: 'listenbrainz****', enabled: true };
	h.targets = { scrobble_to_lastfm: false, scrobble_to_listenbrainz: true };
	h.connPending = false;
	h.targetsPending = false;
	h.connError = false;
	h.saving = false;
	h.verifying = false;
	h.verify.mockResolvedValue({ valid: true, message: 'Connected as needle' });
});

describe('SettingsListenBrainz', () => {
	it('renders the saved connection and scrobble targets', async () => {
		await render(SettingsListenBrainz);

		await expect.element(page.getByLabelText('Username')).toHaveValue('needle');
		await expect.element(page.getByLabelText('User token')).toHaveValue('listenbrainz****');
		await expect.element(page.getByLabelText('Enable ListenBrainz')).toBeChecked();
		await expect.element(page.getByLabelText('Scrobble to ListenBrainz')).toBeChecked();
		await expect.element(page.getByLabelText('Scrobble to Last.fm')).not.toBeChecked();
	});

	it('saves edited values and confirms inline', async () => {
		await render(SettingsListenBrainz);

		await page.getByLabelText('Username').fill('needle2');
		await page.getByRole('button', { name: 'Save ListenBrainz settings' }).click();

		expect(h.saveConn).toHaveBeenCalledWith({
			username: 'needle2',
			user_token: 'listenbrainz****',
			enabled: true
		});
		await expect.element(page.getByText(/settings saved/i)).toBeVisible();
	});

	it('tests the connection and shows the verdict', async () => {
		await render(SettingsListenBrainz);

		await page.getByRole('button', { name: 'Test connection' }).click();

		expect(h.verify).toHaveBeenCalledWith({
			username: 'needle',
			user_token: 'listenbrainz****',
			enabled: true
		});
		await expect.element(page.getByRole('status')).toHaveTextContent('Connected as needle');
	});

	it('saves toggled scrobble targets', async () => {
		await render(SettingsListenBrainz);

		await page.getByLabelText('Scrobble to Last.fm').click();
		await page.getByRole('button', { name: 'Save scrobble targets' }).click();

		expect(h.saveTargets).toHaveBeenCalledWith({
			scrobble_to_lastfm: true,
			scrobble_to_listenbrainz: true
		});
	});

	it('shows a skeleton while the connection loads', async () => {
		h.connPending = true;

		await render(SettingsListenBrainz);

		await expect.element(page.getByTestId('listenbrainz-skeleton')).toBeInTheDocument();
		expect(page.getByLabelText('Username').query()).toBeNull();
	});

	it('shows an honest error with a retry that refetches', async () => {
		h.connError = true;

		await render(SettingsListenBrainz);

		await expect.element(page.getByText(/could not load listenbrainz settings/i)).toBeVisible();
		await page.getByRole('button', { name: /retry/i }).click();
		expect(h.refetchConn).toHaveBeenCalledTimes(1);
	});
});
