import { page } from '@vitest/browser/context';
import { createSubscriber } from 'svelte/reactivity';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { ServiceHealthItem } from '$lib/types';

vi.mock('$env/dynamic/public', () => ({ env: { PUBLIC_API_URL: '' } }));

type QueryData = { degraded: ServiceHealthItem[] };

const queryState = vi.hoisted(() => {
	let data: QueryData = { degraded: [] };
	let notify: (() => void) | undefined;

	return {
		get data(): QueryData {
			return data;
		},
		set data(next: QueryData) {
			data = next;
			notify?.();
		},
		setNotify(listener: (() => void) | undefined) {
			notify = listener;
		}
	};
});

vi.mock('$lib/queries/system/SystemHealthQuery.svelte', () => {
	const subscribe = createSubscriber((update) => {
		queryState.setNotify(update);
		return () => queryState.setNotify(undefined);
	});

	return {
		getSystemHealthQuery: () => ({
			get data(): QueryData {
				subscribe();
				return queryState.data;
			}
		})
	};
});

const toast = vi.hoisted(() => ({ show: vi.fn() }));
vi.mock('$lib/stores/toast', () => ({ toastStore: toast }));

import ServiceHealthIndicator from './ServiceHealthIndicator.svelte';
const START_TIME = 1_000_000;
const NOTIFICATION_COOLDOWN = 10 * 60 * 1000;

function degradedItem(
	service: string,
	capability: string,
	fallback: string | null = null,
	message = `${service} ${capability} is temporarily unavailable.`
): ServiceHealthItem {
	return {
		service,
		capability,
		severity: 'degraded',
		message,
		fallback,
		degraded_seconds: 0
	};
}

describe('ServiceHealthIndicator', () => {
	beforeEach(() => {
		vi.useRealTimers();
		queryState.data = { degraded: [] };
		toast.show.mockClear();
	});

	afterEach(() => {
		vi.useRealTimers();
		vi.restoreAllMocks();
	});

	it('is invisible when nothing is degraded', async () => {
		queryState.data = { degraded: [] };
		await render(ServiceHealthIndicator);
		await expect
			.element(page.getByRole('button', { name: /service status/i }))
			.not.toBeInTheDocument();
	});

	it('shows the dot, toasts once, and reveals details on click', async () => {
		toast.show.mockClear();
		queryState.data = {
			degraded: [
				{
					service: 'listenbrainz',
					capability: 'popularity',
					severity: 'degraded',
					message: "ListenBrainz's popularity data is temporarily unavailable.",
					fallback: 'lastfm',
					degraded_seconds: 120
				}
			]
		};

		await render(ServiceHealthIndicator);

		// first-time toast fired
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));

		// dot is visible; clicking opens the detail popover with the friendly names
		await page.getByRole('button', { name: /service status/i }).click();
		await expect.element(page.getByText('ListenBrainz', { exact: true })).toBeVisible();
		await expect.element(page.getByText(/Using .*Last\.fm.* instead/i)).toBeVisible();
	});

	it('renders friendly labels for the metadata/enrichment services', async () => {
		queryState.data = {
			degraded: [
				{
					service: 'musicbrainz',
					capability: 'metadata',
					severity: 'degraded',
					message: 'MusicBrainz is having issues.',
					fallback: null,
					degraded_seconds: 30
				},
				{
					service: 'wikidata',
					capability: 'artist info',
					severity: 'degraded',
					message: 'Artist bios and images (Wikipedia) are temporarily unavailable.',
					fallback: null,
					degraded_seconds: 10
				}
			]
		};

		await render(ServiceHealthIndicator);

		await page.getByRole('button', { name: /service status/i }).click();
		await expect.element(page.getByText('MusicBrainz', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Wikipedia', { exact: true })).toBeVisible();
	});

	it('toast omits the fallback claim when a degraded service has none', async () => {
		toast.show.mockClear();
		queryState.data = {
			degraded: [
				{
					service: 'musicbrainz',
					capability: 'metadata-without-fallback',
					severity: 'degraded',
					message: 'MusicBrainz is having issues.',
					fallback: null,
					degraded_seconds: 5
				}
			]
		};

		await render(ServiceHealthIndicator);

		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));
		const msg = toast.show.mock.calls[0][0].message as string;
		expect(msg).toContain('auto-retrying');
		expect(msg).not.toContain('fallback');
	});

	it('renders acquisition cleanup debt without exposing a path', async () => {
		toast.show.mockClear();
		queryState.data = {
			degraded: [
				{
					service: 'acquisition_cleanup',
					capability: 'source-files-only',
					severity: 'degraded',
					message:
						"Temporary files couldn't be removed for 2 downloads. Your library is safe. Retrying automatically.",
					fallback: null,
					degraded_seconds: 0
				}
			]
		};

		await render(ServiceHealthIndicator);
		await page.getByRole('button', { name: /service status/i }).click();
		await expect.element(page.getByText('Download cleanup', { exact: true })).toBeVisible();
		await expect
			.element(
				page.getByText(
					"Temporary files couldn't be removed for 2 downloads. Your library is safe. Retrying automatically."
				)
			)
			.toBeVisible();
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));
		expect(toast.show.mock.calls[0][0].message).toContain('Your library is safe.');
	});

	it('does not hide another degraded service behind cleanup debt', async () => {
		toast.show.mockClear();
		queryState.data = {
			degraded: [
				{
					service: 'acquisition_cleanup',
					capability: 'source-files-with-cleanup',
					severity: 'degraded',
					message:
						"Temporary files couldn't be removed for 1 download. Your library is safe. Retrying automatically.",
					fallback: null,
					degraded_seconds: 0
				},
				{
					service: 'musicbrainz',
					capability: 'metadata-with-cleanup',
					severity: 'degraded',
					message: 'MusicBrainz is having issues.',
					fallback: null,
					degraded_seconds: 0
				}
			]
		};

		await render(ServiceHealthIndicator);

		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));
		const message = toast.show.mock.calls[0][0].message as string;
		expect(message).toContain('Download cleanup');
		expect(message).toContain('MusicBrainz');
		expect(message).toContain('are having problems.');
	});
	it('uses singular grammar when cleanup has multiple capabilities', async () => {
		vi.spyOn(Date, 'now').mockReturnValue(START_TIME);
		queryState.data = {
			degraded: [
				degradedItem(
					'acquisition_cleanup',
					'singular-cleanup-first',
					null,
					'Download cleanup has one kind of debt.'
				),
				degradedItem(
					'acquisition_cleanup',
					'singular-cleanup-second',
					null,
					'Download cleanup has another kind of debt.'
				)
			]
		};

		await render(ServiceHealthIndicator);

		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));
		const message = toast.show.mock.calls[0][0].message as string;
		expect(message).toContain('Download cleanup is having problems.');
		expect(message).not.toContain('Download cleanup are');
	});
	it('toasts immediately for a first degraded capability', async () => {
		vi.spyOn(Date, 'now').mockReturnValue(START_TIME);
		queryState.data = { degraded: [degradedItem('lastfm', 'scrobbling')] };

		await render(ServiceHealthIndicator);

		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));
		expect(toast.show.mock.calls[0][0].message).toContain('Last.fm');
	});

	it('does not duplicate a toast when a degraded capability polls again', async () => {
		vi.spyOn(Date, 'now').mockReturnValue(START_TIME);
		const initial = degradedItem(
			'listenbrainz',
			'stable-popularity',
			'lastfm',
			'Popularity is temporarily unavailable.'
		);
		queryState.data = { degraded: [initial] };

		await render(ServiceHealthIndicator);
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));

		queryState.data = {
			degraded: [{ ...initial, message: 'Popularity is still temporarily unavailable.' }]
		};
		await page.getByRole('button', { name: /service status/i }).click();
		await expect
			.element(page.getByText('Popularity is still temporarily unavailable.'))
			.toBeVisible();
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));
	});

	it('does not re-toast a capability that heals and returns inside ten minutes', async () => {
		vi.spyOn(Date, 'now').mockReturnValue(START_TIME);
		const initial = degradedItem('listenbrainz', 'flapping-popularity');
		queryState.data = { degraded: [initial] };

		await render(ServiceHealthIndicator);
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));

		queryState.data = { degraded: [] };
		await expect
			.element(page.getByRole('button', { name: /service status/i }))
			.not.toBeInTheDocument();

		queryState.data = { degraded: [initial] };
		await expect.element(page.getByRole('button', { name: /service status/i })).toBeVisible();
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));
	});
	it('does not re-toast a capability after a component remount', async () => {
		vi.spyOn(Date, 'now').mockReturnValue(START_TIME);
		const initial = degradedItem('listenbrainz', 'remount-popularity');
		queryState.data = { degraded: [initial] };

		const first = await render(ServiceHealthIndicator);
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));
		await first.unmount();

		const second = await render(ServiceHealthIndicator);
		await expect.element(page.getByRole('button', { name: /service status/i })).toBeVisible();
		expect(toast.show).toHaveBeenCalledTimes(1);
		await second.unmount();
	});

	it('does not re-toast a capability just under the ten-minute boundary', async () => {
		const now = vi.spyOn(Date, 'now').mockReturnValue(START_TIME);
		const initial = degradedItem('listenbrainz', 'just-under-boundary-popularity');
		queryState.data = { degraded: [initial] };

		const view = await render(ServiceHealthIndicator);
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));

		now.mockReturnValue(START_TIME + NOTIFICATION_COOLDOWN - 1);
		queryState.data = {
			degraded: [{ ...initial, message: 'Popularity remains unavailable just under ten minutes.' }]
		};
		await page.getByRole('button', { name: /service status/i }).click();
		await expect
			.element(page.getByText('Popularity remains unavailable just under ten minutes.'))
			.toBeVisible();
		expect(toast.show).toHaveBeenCalledTimes(1);
		await view.unmount();
	});

	it('prunes expired notification timestamps before checking eligibility', async () => {
		const now = vi.spyOn(Date, 'now').mockReturnValue(START_TIME);
		const initial = degradedItem('listenbrainz', 'pruned-timestamp-popularity');
		queryState.data = { degraded: [initial] };

		const view = await render(ServiceHealthIndicator);
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));

		now.mockReturnValue(START_TIME + NOTIFICATION_COOLDOWN);
		queryState.data = { degraded: [] };
		await expect
			.element(page.getByRole('button', { name: /service status/i }))
			.not.toBeInTheDocument();

		now.mockReturnValue(START_TIME + NOTIFICATION_COOLDOWN - 1);
		queryState.data = {
			degraded: [
				{
					...initial,
					message: 'Popularity returned just under the cooldown after pruning.'
				}
			]
		};
		await page.getByRole('button', { name: /service status/i }).click();
		await expect
			.element(page.getByText('Popularity returned just under the cooldown after pruning.'))
			.toBeVisible();
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(2));
		await view.unmount();
	});

	it('toasts again when the same capability reaches the ten-minute boundary', async () => {
		const now = vi.spyOn(Date, 'now').mockReturnValue(START_TIME);
		const initial = degradedItem('listenbrainz', 'slow-popularity');
		queryState.data = { degraded: [initial] };

		await render(ServiceHealthIndicator);
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));

		now.mockReturnValue(START_TIME + NOTIFICATION_COOLDOWN);
		queryState.data = {
			degraded: [{ ...initial, message: 'Popularity is unavailable again.' }]
		};
		await page.getByRole('button', { name: /service status/i }).click();
		await expect.element(page.getByText('Popularity is unavailable again.')).toBeVisible();
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(2));
	});

	it('notifies a newly degraded capability while an existing one is debounced', async () => {
		vi.spyOn(Date, 'now').mockReturnValue(START_TIME);
		const existing = degradedItem(
			'listenbrainz',
			'debounced-popularity',
			'lastfm',
			'Popularity remains unavailable.'
		);
		const newlyDegraded = degradedItem(
			'musicbrainz',
			'new-metadata',
			null,
			'Metadata is temporarily unavailable.'
		);
		queryState.data = { degraded: [existing] };

		await render(ServiceHealthIndicator);
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(1));

		queryState.data = {
			degraded: [{ ...existing, message: 'Popularity remains unavailable.' }, newlyDegraded]
		};
		await expect
			.element(page.getByRole('button', { name: 'Service status: 2 degraded' }))
			.toBeVisible();
		await vi.waitFor(() => expect(toast.show).toHaveBeenCalledTimes(2));

		const message = toast.show.mock.calls[1][0].message as string;
		expect(message).toContain('MusicBrainz');
		expect(message).not.toContain('ListenBrainz');

		await page.getByRole('button', { name: /service status/i }).click();
		await expect.element(page.getByText('ListenBrainz', { exact: true })).toBeVisible();
		await expect.element(page.getByText('MusicBrainz', { exact: true })).toBeVisible();
	});
});
