import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import { resetOrganizerRetry } from '$lib/queries/downloads/DownloadSSE.svelte';
import type { HeldImport } from '$lib/types';

const h = vi.hoisted(() => ({
	retry: vi.fn(),
	discard: vi.fn(),
	reset: vi.fn(),
	retryPending: false,
	isAdmin: true,
	invalidate: vi.fn()
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: {
		user: { id: 'user-1' },
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

vi.mock('$lib/queries/QueryClient', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/QueryClient')>()),
	invalidateQueriesWithPersister: (...args: unknown[]) => h.invalidate(...args)
}));

class FakeEventSource {
	static instances: FakeEventSource[] = [];
	url: string;
	listeners: Record<string, ((e: MessageEvent) => void)[]> = {};
	closed = false;

	constructor(url: string) {
		this.url = url;
		FakeEventSource.instances.push(this);
	}

	addEventListener(type: string, cb: (e: MessageEvent) => void) {
		(this.listeners[type] ??= []).push(cb);
	}

	close() {
		this.closed = true;
	}

	emit(type: string, data: unknown) {
		const ev = { data: JSON.stringify(data) } as MessageEvent;
		for (const cb of this.listeners[type] ?? []) cb(ev);
	}
}

import ManagementHoldCard from './ManagementHoldCard.svelte';

async function retryEvents(): Promise<FakeEventSource> {
	await vi.waitFor(() => {
		if (FakeEventSource.instances.length === 0) throw new Error('retry stream not opened yet');
	});
	return FakeEventSource.instances[FakeEventSource.instances.length - 1];
}

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
		h.invalidate = vi.fn().mockResolvedValue(undefined);
		FakeEventSource.instances = [];
		vi.stubGlobal('EventSource', FakeEventSource as unknown as typeof EventSource);
		// The organizerRetry store is module-level: clear the shared task id so no
		// snapshot leaks between tests.
		resetOrganizerRetry('task-1');
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it('presents a secured album as one actionable unit with expandable evidence', async () => {
		await render(ManagementHoldCard, { props: { items: [held(2), held(1)] } } as Parameters<
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
		await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
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
		await render(ManagementHoldCard, { props: { items: [item] } } as Parameters<
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
		await render(ManagementHoldCard, { props: { items: [item] } } as Parameters<
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
		const view = await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
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
		const view = await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await expect.element(page.getByRole('heading', { name: 'Boom. Done.' })).toBeVisible();
		await view.rerender({ items: [] });
		await expect
			.element(page.getByRole('heading', { name: 'Boom. Done.' }))
			.not.toBeInTheDocument();
	});

	it('requires confirmation before discarding every secured file', async () => {
		await render(ManagementHoldCard, { props: { items: [held(1), held(2)] } } as Parameters<
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
		await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
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
		await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
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

	it('swaps the rechecking text for live progress on the first retry event', async () => {
		h.retry.mockImplementation(() => {
			h.retryPending = true;
		});
		await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await page.getByRole('button', { name: 'Retry organizer' }).click();
		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent('Rechecking the secured album…');

		const events = await retryEvents();
		events.emit('organizer_retry', {
			state: 'running',
			stage: 'publishing',
			files_completed: 7,
			files_total: 13,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		await expect.element(page.getByText('Publishing 7/13').first()).toBeVisible();
		await expect.element(page.getByRole('status')).not.toHaveTextContent('Rechecking');
	});

	it('renders retry progress as a native progress element with no custom animation', async () => {
		h.retry.mockImplementation(() => {
			h.retryPending = true;
		});
		await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await page.getByRole('button', { name: 'Retry organizer' }).click();
		const events = await retryEvents();
		events.emit('organizer_retry', {
			state: 'running',
			stage: 'publishing',
			files_completed: 7,
			files_total: 13,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		// Native <progress> (role progressbar) carries the live counts: no custom
		// keyframes, so there is nothing to add to the prefers-reduced-motion blocks.
		await expect.element(page.getByRole('progressbar', { name: 'Publishing 7/13' })).toBeVisible();
	});

	it('shows the imported count when the organizer retry completes', async () => {
		h.retry.mockImplementation(() => {
			h.retryPending = true;
		});
		await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await page.getByRole('button', { name: 'Retry organizer' }).click();
		const events = await retryEvents();
		events.emit('organizer_retry', {
			state: 'complete',
			stage: 'finalizing',
			files_completed: 13,
			files_total: 13,
			files_imported: 13,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		await expect.element(page.getByText('Organizer retry imported 13 files.')).toBeVisible();
	});

	it('uses the singular file copy for a single-file retry', async () => {
		h.retry.mockImplementation(() => {
			h.retryPending = true;
		});
		await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await page.getByRole('button', { name: 'Retry organizer' }).click();
		const events = await retryEvents();
		events.emit('organizer_retry', {
			state: 'complete',
			stage: 'finalizing',
			files_completed: 1,
			files_total: 1,
			files_imported: 1,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		await expect.element(page.getByText('Organizer retry imported 1 file.')).toBeVisible();
	});

	it('settles a duplicate terminal exactly once, keeping the complete copy', async () => {
		h.retry.mockImplementation(() => {
			h.retryPending = true;
		});
		await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await page.getByRole('button', { name: 'Retry organizer' }).click();
		const events = await retryEvents();
		events.emit('organizer_retry', {
			state: 'complete',
			stage: 'finalizing',
			files_completed: 13,
			files_total: 13,
			files_imported: 13,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		await expect.element(page.getByText('Organizer retry imported 13 files.')).toBeVisible();
		events.emit('organizer_retry', {
			state: 'failed',
			stage: 'publishing',
			files_completed: 7,
			files_total: 13,
			error: 'Contradictory late failure',
			updated_at: '2026-09-13T22:00:01+00:00'
		});
		await expect.element(page.getByText('Organizer retry imported 13 files.')).toBeVisible();
		await expect.element(page.getByText('Contradictory late failure')).not.toBeInTheDocument();
		await expect.element(page.getByRole('alert')).not.toBeInTheDocument();
	});

	it('settles once when the mutation response follows the terminal event', async () => {
		let capturedOptions: { onSuccess: (data: { files: number }) => void } | undefined;
		h.retry.mockImplementation((_input, options) => {
			h.retryPending = true;
			capturedOptions = options;
		});
		await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await page.getByRole('button', { name: 'Retry organizer' }).click();
		const events = await retryEvents();
		events.emit('organizer_retry', {
			state: 'complete',
			stage: 'finalizing',
			files_completed: 13,
			files_total: 13,
			files_imported: 13,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		await expect.element(page.getByText('Organizer retry imported 13 files.')).toBeVisible();
		h.retryPending = false;
		capturedOptions?.onSuccess({ files: 13 });
		await expect.element(page.getByText('Organizer retry imported 13 files.')).toBeVisible();
		await expect.element(page.getByRole('alert')).not.toBeInTheDocument();
	});

	it('surfaces an already-running 409 with the backend message inline', async () => {
		h.retry.mockImplementation((_input, options) => {
			options.onError(new Error('An organizer retry is already running for this album.'));
		});
		await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		await page.getByRole('button', { name: 'Retry organizer' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('An organizer retry is already running for this album.');
	});

	it('re-attaches to a running retry on mount without starting a duplicate', async () => {
		await render(ManagementHoldCard, { props: { items: [held(1)] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		const events = await retryEvents();
		events.emit('organizer_retry', {
			state: 'running',
			stage: 'publishing',
			files_completed: 5,
			files_total: 13,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		await expect.element(page.getByText('Publishing 5/13').first()).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Retry in progress…' })).toBeDisabled();
		expect(h.retry).not.toHaveBeenCalled();
	});

	it('settles a foreign terminal snapshot silently with invalidation and no copy', async () => {
		const item = { ...held(1), release_group_mbid: 'rg-1' };
		await render(ManagementHoldCard, { props: { items: [item] } } as Parameters<
			typeof render<typeof ManagementHoldCard>
		>[1]);

		const events = await retryEvents();
		events.emit('organizer_retry', {
			state: 'complete',
			stage: 'finalizing',
			files_completed: 13,
			files_total: 13,
			files_imported: 13,
			updated_at: '2026-09-13T22:00:00+00:00'
		});
		await vi.waitFor(() => {
			expect(h.invalidate).toHaveBeenCalledWith(
				expect.objectContaining({
					queryKey: expect.arrayContaining(['downloads', 'tasks', 'user-1'])
				})
			);
			expect(h.invalidate).toHaveBeenCalledWith(
				expect.objectContaining({ queryKey: ['library', 'stats'] })
			);
			expect(h.invalidate).toHaveBeenCalledWith(
				expect.objectContaining({ queryKey: ['library', 'recently-added'] })
			);
			expect(h.invalidate).toHaveBeenCalledWith(
				expect.objectContaining({ queryKey: ['library', 'album', 'rg-1'] })
			);
		});
		await expect.element(page.getByRole('alert')).not.toBeInTheDocument();
		await expect.element(page.getByText('Organizer retry imported')).not.toBeInTheDocument();
	});
});
