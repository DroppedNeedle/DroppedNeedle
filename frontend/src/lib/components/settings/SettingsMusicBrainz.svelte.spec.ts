import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, describe, expect, it, type Mock, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type {
	MusicBrainzSettingsResponse,
	MusicBrainzSettingsUpdate
} from '$lib/queries/musicbrainz/types';

const h = vi.hoisted(() => {
	const data: MusicBrainzSettingsResponse = {
		source_mode: 'brainzmash',
		api_url: 'https://api.brainzmash.cc/ws/2',
		rate_limit: 10,
		concurrent_searches: 1,
		community_acknowledged: null,
		selected_source_mode: 'brainzmash',
		source_id: 'brainzmash-default',
		generation: 1,
		pending_brainzmash: null,
		clamped_to_official_limits: false
	};
	const mutation = () => ({
		isPending: false,
		mutateAsync: vi.fn()
	});
	const state = {
		data,
		query: {
			get data() {
				return state.data;
			},
			isLoading: false,
			isError: false,
			error: null
		},
		save: mutation(),
		consent: mutation(),
		stage: mutation(),
		verify: mutation(),
		activate: mutation(),
		invalidate: vi.fn().mockResolvedValue(undefined),
		clearCaches: vi.fn().mockReturnValue(true),
		lastSettingsUpdate: null as MusicBrainzSettingsUpdate | null
	};
	return state;
});

vi.mock('$lib/queries/musicbrainz/MusicBrainzQueries.svelte', () => ({
	getMusicBrainzSettingsQuery: () => h.query
}));
vi.mock('$lib/queries/musicbrainz/MusicBrainzMutations.svelte', () => ({
	saveMusicBrainzSettings: () => h.save,
	consentBrainzMash: () => h.consent,
	stageBrainzMash: () => h.stage,
	testMusicBrainzConnection: () => h.verify,
	activateBrainzMash: () => h.activate
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateMusicBrainzProviderQueries: h.invalidate
}));

vi.mock('$lib/utils/albumDetailCache', () => ({
	clearMusicBrainzProviderCaches: h.clearCaches
}));

import SettingsMusicBrainz from './SettingsMusicBrainz.svelte';

function settings(
	overrides: Partial<MusicBrainzSettingsResponse> = {}
): MusicBrainzSettingsResponse {
	return {
		...h.data,
		...overrides,
		pending_brainzmash: overrides.pending_brainzmash
			? { ...overrides.pending_brainzmash }
			: overrides.pending_brainzmash === null
				? null
				: h.data.pending_brainzmash
	};
}

function binding(
	overrides: Partial<NonNullable<MusicBrainzSettingsResponse['pending_brainzmash']>> = {}
) {
	return {
		access_revision: 'access-1',
		source_id: 'source-1',
		generation: 1,
		disclosure_version: '2026-08-31',
		...overrides
	};
}

function initialSettings(): MusicBrainzSettingsResponse {
	return {
		source_mode: 'brainzmash',
		api_url: 'https://api.brainzmash.cc/ws/2',
		rate_limit: 10,
		concurrent_searches: 1,
		community_acknowledged: null,
		selected_source_mode: 'brainzmash',
		source_id: 'brainzmash-default',
		generation: 1,
		pending_brainzmash: null,
		clamped_to_official_limits: false
	};
}

function quarantinedSettings(
	mode: 'official' | 'mirror' | 'community'
): MusicBrainzSettingsResponse {
	return settings({
		source_mode: 'brainzmash',
		selected_source_mode: mode,
		api_url: 'https://api.brainzmash.cc/ws/2',
		source_id: 'quarantine-source',
		generation: 12,
		active_brainzmash: {
			endpoint: 'https://api.brainzmash.cc/ws/2',
			access_revision: 'access-quarantine',
			source_id: 'quarantine-source',
			generation: 12,
			disclosure_version: '2026-08-31',
			consented: true,
			verified: true
		},
		pending_brainzmash: {
			endpoint: 'https://api.brainzmash.cc/ws/2',
			access_revision: 'access-pending',
			source_id: 'pending-source',
			generation: 13,
			disclosure_version: '2026-08-31',
			consented: false,
			verified: false
		},
		source_quarantined: true,
		quarantine_reason: 'Existing source settings require review.'
	});
}

function resetMutation(mutation: { isPending: boolean; mutateAsync: Mock }) {
	mutation.isPending = false;
	mutation.mutateAsync.mockReset();
	mutation.mutateAsync.mockResolvedValue(h.data);
}

beforeEach(() => {
	h.data = initialSettings();
	h.lastSettingsUpdate = null;
	resetMutation(h.save);
	resetMutation(h.consent);
	resetMutation(h.stage);
	resetMutation(h.verify);
	resetMutation(h.activate);
	h.invalidate.mockReset();
	h.invalidate.mockResolvedValue(undefined);
	h.clearCaches.mockReset();
	h.clearCaches.mockReturnValue(true);
});

afterEach(async () => {
	await page.viewport(1280, 720);
});

describe('MusicBrainz four-way source picker', () => {
	it('renders four same-name native radios with BrainzMash recommended and active by default', async () => {
		render(SettingsMusicBrainz);

		const radios = page.getByRole('radio');
		expect(radios.all()).toHaveLength(4);
		await expect.element(page.getByRole('radio', { name: 'BrainzMash' })).toBeChecked();
		await expect.element(page.getByRole('radio', { name: 'Official' })).not.toBeChecked();
		await expect.element(page.getByText('Recommended', { exact: true })).toBeVisible();
		await expect.element(page.getByText(/BrainzMash is the active runtime source/)).toBeVisible();
		await expect
			.element(page.getByText('Built-in endpoint: https://api.brainzmash.cc/ws/2'))
			.toBeVisible();
	});

	it('does not show Official as active while the built-in BrainzMash source is selected', async () => {
		render(SettingsMusicBrainz);

		await expect.element(page.getByTestId('active-source-preserved')).not.toBeInTheDocument();
		await expect.element(page.getByText(/Active source: Official/)).not.toBeInTheDocument();
		await expect.element(page.getByText(/BrainzMash is the active runtime source/)).toBeVisible();
	});

	it('keeps optional disclosure metadata separate from the active BrainzMash source', async () => {
		h.data = settings({
			source_mode: 'brainzmash',
			selected_source_mode: 'brainzmash',
			api_url: 'https://api.brainzmash.cc/ws/2',
			source_id: 'brainzmash-active',
			generation: 7,
			active_brainzmash: {
				endpoint: 'https://api.brainzmash.cc/ws/2',
				access_revision: 'access-active',
				source_id: 'brainzmash-active',
				generation: 7,
				disclosure_version: '2026-08-31',
				consented: true,
				verified: true
			},
			pending_brainzmash: {
				endpoint: 'https://api.brainzmash.cc/ws/2',
				access_revision: 'access-pending',
				source_id: 'brainzmash-pending',
				generation: 8,
				disclosure_version: '2026-08-31',
				consented: false,
				verified: false
			}
		});
		render(SettingsMusicBrainz);

		await expect.element(page.getByTestId('active-brainzmash-binding')).toBeVisible();
		await expect.element(page.getByText(/Optional disclosure metadata/)).toBeVisible();
		await expect.element(page.getByTestId('pending-brainzmash-disabled')).toBeVisible();
		await expect.element(page.getByText(/optional disclosure binding is reviewed/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Test Connection' })).toBeDisabled();
	});

	it('does not advertise a quarantined BrainzMash binding as active', async () => {
		h.data = quarantinedSettings('official');
		render(SettingsMusicBrainz);

		await expect.element(page.getByTestId('musicbrainz-quarantined')).toBeVisible();
		await expect.element(page.getByTestId('active-brainzmash-binding')).not.toBeInTheDocument();
		await expect.element(page.getByText('https://musicbrainz.org/ws/2')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Save Settings' })).toBeDisabled();
	});

	it.each([
		['mirror', 'Mirror API Endpoint URL'],
		['community', 'Community Server API Endpoint URL']
	] as const)('does not seed the quarantine BrainzMash URL for %s', async (mode, label) => {
		h.data = quarantinedSettings(mode);
		render(SettingsMusicBrainz);

		await expect.element(page.getByTestId('musicbrainz-quarantined')).toBeVisible();
		await expect.element(page.getByTestId('active-brainzmash-binding')).not.toBeInTheDocument();
		await expect.element(page.getByRole('textbox', { name: label })).toHaveValue('');
		await expect
			.element(page.getByRole('textbox', { name: label }))
			.not.toHaveValue('https://api.brainzmash.cc/ws/2');
		await expect.element(page.getByRole('button', { name: 'Save Settings' })).toBeDisabled();
	});

	it('makes a quarantined community rollback actionable after endpoint and risk checks', async () => {
		h.data = quarantinedSettings('community');
		h.save.mutateAsync.mockResolvedValueOnce(
			settings({
				source_mode: 'community',
				selected_source_mode: 'community',
				api_url: 'https://community.example/ws/2',
				community_acknowledged: true,
				active_brainzmash: null,
				pending_brainzmash: null,
				source_quarantined: false,
				quarantine_reason: ''
			})
		);
		render(SettingsMusicBrainz);

		const endpoint = page.getByRole('textbox', { name: 'Community Server API Endpoint URL' });
		await expect.element(page.getByRole('button', { name: 'Save Settings' })).toBeDisabled();
		await endpoint.fill('https://community.example/ws/2');
		await expect.element(page.getByRole('button', { name: 'Save Settings' })).toBeDisabled();
		await page.getByRole('checkbox', { name: /I understand the risks/ }).click();
		await expect.element(page.getByRole('button', { name: 'Save Settings' })).toBeDisabled();

		await page.getByRole('button', { name: 'Test Connection' }).click();
		expect(h.verify.mutateAsync).toHaveBeenCalledWith({
			source_mode: 'community',
			api_url: 'https://community.example/ws/2',
			rate_limit: 1,
			concurrent_searches: 1,
			community_acknowledged: true
		});
		await expect.element(page.getByRole('button', { name: 'Save Settings' })).toBeEnabled();

		await page.getByRole('button', { name: 'Save Settings' }).click();
		expect(h.save.mutateAsync).toHaveBeenCalledWith({
			source_mode: 'community',
			api_url: 'https://community.example/ws/2',
			rate_limit: 1,
			concurrent_searches: 1,
			community_acknowledged: true
		});
		await expect.element(page.getByText('MusicBrainz settings saved.')).toBeVisible();
	});

	it('keeps BrainzMash active without consent, verification, or activation staging', async () => {
		const active = settings({
			source_mode: 'brainzmash',
			selected_source_mode: 'brainzmash',
			api_url: 'https://api.brainzmash.cc/ws/2',
			pending_brainzmash: null
		});
		h.stage.mutateAsync.mockResolvedValueOnce(active);
		render(SettingsMusicBrainz);

		await page.getByRole('button', { name: 'Reset to Defaults' }).click();
		expect(h.stage.mutateAsync).toHaveBeenCalledWith();
		expect(h.save.mutateAsync).not.toHaveBeenCalled();
		await expect.element(page.getByText(/BrainzMash is active\./)).toBeVisible();
		expect(h.consent.mutateAsync).not.toHaveBeenCalled();
		expect(h.verify.mutateAsync).not.toHaveBeenCalled();
		expect(h.activate.mutateAsync).not.toHaveBeenCalled();
	});

	it('uses the optional disclosure flow only when a pending proposal is supplied', async () => {
		const staged = settings({
			pending_brainzmash: {
				endpoint: 'https://api.brainzmash.cc/ws/2',
				access_revision: 'access-2',
				source_id: 'source-2',
				generation: 2,
				disclosure_version: '2026-08-31',
				consented: false,
				verified: false
			}
		});
		const consented = settings({
			pending_brainzmash: { ...staged.pending_brainzmash!, consented: true }
		});
		const verified = settings({
			pending_brainzmash: { ...consented.pending_brainzmash!, verified: true }
		});
		const active = settings({
			source_mode: 'brainzmash',
			selected_source_mode: 'brainzmash',
			api_url: 'https://api.brainzmash.cc/ws/2',
			pending_brainzmash: null
		});
		h.data = staged;
		h.consent.mutateAsync.mockResolvedValueOnce(consented);
		h.verify.mutateAsync.mockResolvedValueOnce(verified);
		h.activate.mutateAsync.mockResolvedValueOnce(active);

		render(SettingsMusicBrainz);
		// A pending proposal is visible as optional disclosure metadata; runtime
		// BrainzMash remains selected and active throughout the flow.
		await expect
			.element(page.getByRole('checkbox', { name: /Accept BrainzMash privacy/ }))
			.toBeVisible();
		await page.getByRole('checkbox', { name: /Accept BrainzMash privacy/ }).click();
		expect(h.consent.mutateAsync).toHaveBeenCalledWith(
			binding({
				access_revision: 'access-2',
				source_id: 'source-2',
				generation: 2,
				disclosure_version: '2026-08-31'
			})
		);

		await page.getByRole('button', { name: 'Test Connection' }).click();
		expect(h.verify.mutateAsync).toHaveBeenCalledWith(
			binding({
				access_revision: 'access-2',
				source_id: 'source-2',
				generation: 2,
				disclosure_version: '2026-08-31'
			})
		);
		await page.getByRole('button', { name: 'Activate BrainzMash' }).click();
		expect(h.activate.mutateAsync).toHaveBeenCalledWith(
			binding({
				access_revision: 'access-2',
				source_id: 'source-2',
				generation: 2,
				disclosure_version: '2026-08-31'
			})
		);
		await expect.element(page.getByText('BrainzMash is active.')).toBeVisible();
	});

	it('supports native arrow-key movement and Enter selection', async () => {
		render(SettingsMusicBrainz);
		const brainzMash = page
			.getByRole('radio', { name: 'BrainzMash' })
			.element() as HTMLInputElement;
		brainzMash.focus();
		brainzMash.dispatchEvent(
			new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true })
		);
		await expect.element(page.getByRole('radio', { name: 'Official' })).toBeChecked();
		const mirror = page
			.getByRole('radio', { name: 'Self-hosted mirror' })
			.element() as HTMLInputElement;
		mirror.focus();
		mirror.dispatchEvent(
			new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })
		);
		await expect.element(page.getByRole('radio', { name: 'Self-hosted mirror' })).toBeChecked();
	});

	it('shows only source-specific controls', async () => {
		render(SettingsMusicBrainz);
		await expect.element(page.getByRole('spinbutton')).not.toBeInTheDocument();
		await page.getByText('More info: BrainzMash', { exact: true }).click();
		await expect.element(page.getByText(/local wire policy/).last()).toBeVisible();
		await page.getByRole('radio', { name: 'Self-hosted mirror' }).click();
		await expect
			.element(page.getByRole('textbox', { name: 'Mirror API Endpoint URL' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('spinbutton', { name: 'Rate Limit (requests/sec)' }))
			.toHaveValue('1');
		await expect
			.element(page.getByRole('spinbutton', { name: 'Concurrent Searches' }))
			.toHaveValue('1');
		await expect
			.element(page.getByRole('spinbutton', { name: 'Concurrent Searches' }))
			.toHaveAttribute('max', '64');

		await page.getByRole('radio', { name: 'Official' }).click();
		await expect
			.element(page.getByRole('textbox', { name: 'Mirror API Endpoint URL' }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('textbox', { name: 'Community Server API Endpoint URL' }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('spinbutton', { name: 'Rate Limit (requests/sec)' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('spinbutton', { name: 'Concurrent Searches' }))
			.toHaveAttribute('max', '1');

		await page.getByRole('radio', { name: 'Community / external server' }).click();
		await expect
			.element(page.getByRole('checkbox', { name: /I understand the risks/ }))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Save Settings' })).toBeDisabled();
	});

	it('keeps cards in one column on mobile and a 2x2 grid at md', async () => {
		await page.viewport(390, 760);
		render(SettingsMusicBrainz);
		const grid = page.getByTestId('musicbrainz-source-grid');
		await expect.element(grid).toHaveClass(/grid-cols-1/);
		await expect.element(grid).toHaveClass(/md:grid-cols-2/);
		expect(page.getByTestId('musicbrainz-card-brainzmash').all()).toHaveLength(1);
		expect(page.getByTestId('musicbrainz-card-official').all()).toHaveLength(1);
		expect(page.getByTestId('musicbrainz-card-mirror').all()).toHaveLength(1);
		expect(page.getByTestId('musicbrainz-card-community').all()).toHaveLength(1);

		await page.viewport(1280, 720);
		await expect.element(grid).toHaveClass(/grid-cols-1/);
		await expect.element(grid).toHaveClass(/md:grid-cols-2/);
	});

	it('shows BrainzMash staging failures as a role alert without fallback', async () => {
		h.stage.mutateAsync.mockRejectedValueOnce(new Error('stage rejected'));
		h.data = settings({ pending_brainzmash: null });
		render(SettingsMusicBrainz);
		await page.getByRole('button', { name: 'Reset to Defaults' }).click();
		await expect.element(page.getByRole('alert')).toHaveTextContent('stage rejected');
		await expect.element(page.getByText(/Official/).first()).toBeVisible();
	});

	it('switches away from active BrainzMash with one save and no alternate probe', async () => {
		const activeBrainz = settings({
			source_mode: 'brainzmash',
			selected_source_mode: 'brainzmash',
			api_url: 'https://api.brainzmash.cc/ws/2',
			source_id: 'brainzmash-active',
			generation: 2,
			active_brainzmash: {
				endpoint: 'https://api.brainzmash.cc/ws/2',
				access_revision: 'access-active',
				source_id: 'brainzmash-active',
				generation: 2,
				disclosure_version: '2026-08-31',
				consented: true,
				verified: true
			},
			pending_brainzmash: null
		});
		const switchedBrainz = settings({
			source_mode: 'brainzmash',
			selected_source_mode: 'brainzmash',
			api_url: 'https://api.brainzmash.cc/ws/2',
			source_id: 'brainzmash-next',
			generation: 3,
			pending_brainzmash: null
		});
		h.data = activeBrainz;
		h.save.mutateAsync.mockResolvedValueOnce(switchedBrainz);
		render(SettingsMusicBrainz);

		await page.getByRole('radio', { name: 'Official' }).click();
		await expect.element(page.getByRole('button', { name: 'Test Connection' })).toBeDisabled();
		await expect.element(page.getByTestId('brainzmash-no-alternate-test')).toBeVisible();
		await page.getByRole('button', { name: 'Save Settings' }).click();

		expect(h.verify.mutateAsync).not.toHaveBeenCalled();
		expect(h.save.mutateAsync).toHaveBeenCalledWith({
			source_mode: 'official',
			api_url: 'https://musicbrainz.org/ws/2',
			rate_limit: 1,
			concurrent_searches: 1,
			community_acknowledged: null
		});
		await expect.element(page.getByText('MusicBrainz settings saved.')).toBeVisible();
	});

	it('invalidates provider memory and persisted caches only after an active source change', async () => {
		const current = settings({
			source_mode: 'mirror',
			selected_source_mode: 'mirror',
			api_url: 'http://mirror.test/ws/2',
			source_id: 'mirror-source',
			generation: 4,
			pending_brainzmash: null
		});
		const next = settings({
			source_mode: 'brainzmash',
			selected_source_mode: 'brainzmash',
			api_url: 'https://api.brainzmash.cc/ws/2',
			source_id: 'brainzmash-next',
			generation: 5,
			pending_brainzmash: null
		});
		h.data = current;
		h.save.mutateAsync.mockResolvedValueOnce(next);
		render(SettingsMusicBrainz);
		await page.getByRole('radio', { name: 'Official' }).click();
		await page.getByRole('button', { name: 'Test Connection' }).click();
		await expect.element(page.getByText('MusicBrainz connection verified.')).toBeVisible();
		await page.getByRole('button', { name: 'Save Settings' }).click();
		expect(h.save.mutateAsync).toHaveBeenCalledWith({
			source_mode: 'official',
			api_url: 'https://musicbrainz.org/ws/2',
			rate_limit: 1,
			concurrent_searches: 1,
			community_acknowledged: null
		});
		expect(h.invalidate).toHaveBeenCalledOnce();
		expect(h.clearCaches).toHaveBeenCalledOnce();
	});
	it('sweeps provider caches when source generation changes on the same endpoint', async () => {
		const current = settings({
			source_mode: 'mirror',
			selected_source_mode: 'mirror',
			api_url: 'http://mirror.test/ws/2',
			source_id: 'mirror-source',
			generation: 10,
			pending_brainzmash: null
		});
		const next = { ...current, generation: 11 };
		h.data = current;
		h.verify.mutateAsync.mockResolvedValueOnce(current);
		h.save.mutateAsync.mockResolvedValueOnce(next);
		render(SettingsMusicBrainz);

		await page.getByRole('button', { name: 'Test Connection' }).click();
		await expect.element(page.getByText('MusicBrainz connection verified.')).toBeVisible();
		await page.getByRole('button', { name: 'Save Settings' }).click();
		await expect.element(page.getByText('MusicBrainz settings saved.')).toBeVisible();

		expect(h.invalidate).toHaveBeenCalledOnce();
		expect(h.clearCaches).toHaveBeenCalledOnce();
	});
});
