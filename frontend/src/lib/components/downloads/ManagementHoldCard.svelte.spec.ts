import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { HeldImport } from '$lib/types';

const h = vi.hoisted(() => ({
	retry: vi.fn(),
	discard: vi.fn(),
	reset: vi.fn(),
	retryPending: false,
	isAdmin: true
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: {
		get isAdmin() {
			return h.isAdmin;
		}
	}
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	retryHeldManagementUnit: () => ({
		mutate: (...args: unknown[]) => h.retry(...args),
		reset: h.reset,
		get isPending() {
			return h.retryPending;
		}
	}),
	discardHeldManagementUnit: () => ({ mutate: h.discard, isPending: false })
}));

import ManagementHoldCard from './ManagementHoldCard.svelte';

function held(track: number): HeldImport {
	return {
		id: track,
		release_group_mbid: null,
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: `recording-${track}`,
		track_number: track,
		disc_number: 1,
		track_title: `Track ${track}`,
		artist_name: 'Anthony Green',
		album_title: 'Boom. Done.',
		year: 2022,
		original_filename: `${track}.flac`,
		file_format: 'flac',
		duration_seconds: 180,
		expected_duration_seconds: null,
		reason: 'management:PROFILE_CHANGED',
		reason_detail: 'The selected profile changed while this album was being prepared.',
		source: 'soulseek',
		source_task_id: 'task-1',
		created_at: track,
		evidence_title: null,
		evidence_artist: null,
		evidence_score: null,
		management_retry_count: 0,
		management_next_retry_at: null
	};
}

describe('ManagementHoldCard.svelte', () => {
	beforeEach(() => {
		h.retry = vi.fn();
		h.discard = vi.fn();
		h.reset = vi.fn();
		h.retryPending = false;
		h.isAdmin = true;
	});

	it('presents a secured album as one actionable unit with expandable evidence', async () => {
		render(ManagementHoldCard, { props: { items: [held(2), held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await expect.element(page.getByText('Download secured · organizer paused')).toBeVisible();
		await expect.element(page.getByText('2 files safely held')).toBeVisible();
		await page.getByRole('button', { name: 'Show secured files and technical detail' }).click();
		await expect.element(page.getByText('Track 1')).toBeVisible();
		await expect.element(page.getByText('PROFILE_CHANGED')).toBeVisible();
		await expect
			.element(page.getByText('The selected profile changed while this album was being prepared.'))
			.toBeVisible();

		await page.getByRole('button', { name: 'Retry organizer' }).click();
		expect(h.retry).toHaveBeenCalledWith(
			{ taskId: 'task-1', releaseGroupMbid: null },
			expect.objectContaining({ onError: expect.any(Function) })
		);
		expect(h.reset).toHaveBeenCalledOnce();
	});

	it('shows retry progress inside the held card', async () => {
		h.retry.mockImplementation(() => {
			h.retryPending = true;
		});
		render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await page.getByRole('button', { name: 'Retry organizer' }).click();
		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent('Rechecking the secured album…');
	});

	it('explains a rejected tagging or naming script without calling it a config race', async () => {
		const item = {
			...held(1),
			reason: 'management:SCRIPT_VALIDATION_FAILED'
		};
		render(ManagementHoldCard, { props: { items: [item] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await expect
			.element(
				page.getByText(
					"The active profile's tagging or naming rules could not process this file safely."
				)
			)
			.toBeVisible();
	});

	it('shows the durable automatic retry without disabling manual retry', async () => {
		const item = {
			...held(1),
			management_retry_count: 1,
			management_next_retry_at: new Date('2026-08-04T14:15:00').getTime() / 1000
		};
		render(ManagementHoldCard, { props: { items: [item] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent('Automatic organizer retry scheduled');
		await expect.element(page.getByRole('button', { name: 'Retry organizer' })).toBeEnabled();
	});

	it('shows a rejected retry immediately and replaces stale detail after refresh', async () => {
		h.retry.mockImplementation((_input, options) => {
			options.onError(new Error('Exact edition proof is incomplete.'));
		});
		const view = render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await page.getByRole('button', { name: 'Retry organizer' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Exact edition proof is incomplete.');

		const refreshed = {
			...held(1),
			reason: 'management:TRACK_NOT_MAPPED',
			reason_detail: 'Disc 1, track 1 conflicts with the selected edition.'
		};
		await view.rerender({ items: [refreshed] });
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Disc 1, track 1 conflicts with the selected edition.');
		await expect.element(page.getByRole('alert')).not.toHaveTextContent('proof is incomplete');
	});

	it('removes the resolved card as soon as refreshed held data is empty', async () => {
		const view = render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await expect.element(page.getByRole('heading', { name: 'Boom. Done.' })).toBeVisible();
		await view.rerender({ items: [] });
		await expect
			.element(page.getByRole('heading', { name: 'Boom. Done.' }))
			.not.toBeInTheDocument();
	});

	it('requires confirmation before discarding every secured file', async () => {
		render(ManagementHoldCard, { props: { items: [held(1), held(2)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await page.getByRole('button', { name: 'Discard download' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Discard this downloaded album?' }))
			.toBeVisible();
		await page.getByRole('button', { name: 'Discard secured files' }).click();
		expect(h.discard).toHaveBeenCalledWith(
			{ taskId: 'task-1', releaseGroupMbid: null },
			expect.objectContaining({ onSuccess: expect.any(Function) })
		);
	});

	it('keeps destructive organizer controls admin-only', async () => {
		h.isAdmin = false;
		render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await expect
			.element(
				page.getByText('An administrator can retry, discard, or review this organizer hold.')
			)
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Retry organizer' }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('link', { name: 'Review automation' }))
			.not.toBeInTheDocument();
	});

	it('keeps a failed discard visible in the confirmation dialog', async () => {
		h.discard.mockImplementation((_input, options) => {
			options.onError(new Error('The secured files are still in use.'));
		});
		render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await page.getByRole('button', { name: 'Discard download' }).click();
		await page.getByRole('button', { name: 'Discard secured files' }).click();

		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('The secured files are still in use.');
		await expect
			.element(page.getByRole('heading', { name: 'Discard this downloaded album?' }))
			.toBeVisible();
	});
});
