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
	it('does not advertise a quarantined BrainzMash binding as active', async () => {
		h.data = quarantinedSettings('official');
		await render(SettingsMusicBrainz);

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
		await render(SettingsMusicBrainz);

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
		await render(SettingsMusicBrainz);

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
		await render(SettingsMusicBrainz);

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

		await render(SettingsMusicBrainz);
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
		await render(SettingsMusicBrainz);
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
		await render(SettingsMusicBrainz);

		await page.getByRole('button', { name: 'Test Connection' }).click();
		await expect.element(page.getByText('MusicBrainz connection verified.')).toBeVisible();
		await page.getByRole('button', { name: 'Save Settings' }).click();
		await expect.element(page.getByText('MusicBrainz settings saved.')).toBeVisible();

		expect(h.invalidate).toHaveBeenCalledOnce();
		expect(h.clearCaches).toHaveBeenCalledOnce();
	});
});
