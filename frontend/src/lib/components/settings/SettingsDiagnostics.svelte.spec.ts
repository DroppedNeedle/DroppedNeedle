import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

// real Tailwind/daisyUI styles: the skeleton placeholders are asserted visible
import '../../../app.css';

import type { ProviderStats, QueueStats } from '$lib/queries/diagnostics/types';

interface QueryFixture<T> {
	data: T | null;
	isLoading: boolean;
	error: Error | null;
}

const h = vi.hoisted(() => ({
	queueState: {
		data: null,
		isLoading: true,
		error: null
	} as unknown as Record<string, unknown>,
	providerState: {
		data: null,
		isLoading: true,
		error: null
	} as unknown as Record<string, unknown>
}));

vi.mock('$lib/queries/diagnostics/DiagnosticsQueries.svelte', () => ({
	getQueueStatsQuery: () => h.queueState,
	getProviderStatsQuery: () => h.providerState
}));

import SettingsDiagnostics from './SettingsDiagnostics.svelte';

const QUEUE_STATS: QueueStats = {
	stats: {
		user_slots_available: 7,
		image_slots_available: 18,
		background_slots_available: 1,
		user_active: true,
		background_waiters: 5
	}
};

const PROVIDER_STATS: ProviderStats = {
	providers: [
		{
			provider: 'musicbrainz',
			priority: 'user',
			outcome: 'ok',
			count_total: 128,
			rate_per_min_window: 6.4
		},
		{
			provider: 'lastfm',
			priority: 'background',
			outcome: 'http_error',
			count_total: 3,
			rate_per_min_window: 0.04
		}
	],
	window_seconds: 3600,
	counters_since: null,
	body_byte_scope: 'HTTP response bodies; excludes headers and TLS',
	detailed_series_limit: 1024,
	process_epoch: 'fixture-epoch'
};

function setQueryStates(queue: QueryFixture<QueueStats>, provider: QueryFixture<ProviderStats>) {
	h.queueState = { ...queue } as Record<string, unknown>;
	h.providerState = { ...provider } as Record<string, unknown>;
}

describe('SettingsDiagnostics', () => {
	beforeEach(() => {
		setQueryStates(
			{ data: null, isLoading: true, error: null },
			{ data: null, isLoading: true, error: null }
		);
	});

	it('shows skeleton placeholders while the gauges load', async () => {
		await render(SettingsDiagnostics);

		await expect.element(page.getByLabelText('Loading queue gauges')).toBeVisible();
		await expect.element(page.getByLabelText('Loading provider stats')).toBeVisible();
	});

	it('renders queue lane gauges and the provider table with human-readable labels', async () => {
		setQueryStates(
			{ data: QUEUE_STATS, isLoading: false, error: null },
			{ data: PROVIDER_STATS, isLoading: false, error: null }
		);
		await render(SettingsDiagnostics);

		const queues = page.getByRole('region', { name: 'Outbound request queues' });
		await expect.element(queues.getByText('User requests')).toBeVisible();
		await expect.element(queues.getByText('In use now')).toBeVisible();
		await expect.element(queues.getByText('5 waiting for a slot')).toBeVisible();

		await expect.element(page.getByText('MusicBrainz')).toBeVisible();
		await expect.element(page.getByRole('cell', { name: 'Last.fm' })).toBeVisible();
		await expect.element(page.getByRole('cell', { name: 'HTTP error' })).toBeVisible();
		await expect.element(page.getByRole('cell', { name: '<0.1/min' })).toBeVisible();

		const musicbrainzRow = page.getByRole('row', { name: /MusicBrainz User requests OK/ });
		await expect.element(musicbrainzRow).toHaveTextContent('128');
		await expect.element(musicbrainzRow).toHaveTextContent('6.4/min');
	});

	it('states plainly that counters reset on restart and are per-process', async () => {
		await render(SettingsDiagnostics);

		await expect
			.element(page.getByText(/reset\s+whenever\s+the\s+server\s+restarts/))
			.toBeVisible();
		await expect.element(page.getByText(/single\s+worker\s+process\s+only/)).toBeVisible();
	});

	it('surfaces per-panel load failures instead of an empty table', async () => {
		setQueryStates(
			{ data: QUEUE_STATS, isLoading: false, error: null },
			{ data: null, isLoading: false, error: new Error('boom') }
		);
		await render(SettingsDiagnostics);
		const alerts = await page.getByRole('alert').all();
		expect(alerts.length).toBe(1);
		await expect.element(alerts[0]).toHaveTextContent("Couldn't load provider stats.");
	});
});
