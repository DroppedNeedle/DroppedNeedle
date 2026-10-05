import { page } from '@vitest/browser/context';
import { createSubscriber } from 'svelte/reactivity';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { ServiceHealthItem } from '$lib/types';

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
});
