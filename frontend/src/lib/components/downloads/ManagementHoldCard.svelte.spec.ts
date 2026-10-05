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

vi.mock('$lib/queries/QueryClient', () => {
	// musicSource/userSessionCleanup ride along via $lib/constants and import
	// the client value + setter; they only run at call time, so a small
	// in-memory client is enough.
	const store = new Map<string, unknown>();
	const keyOf = (key: unknown) => JSON.stringify(key);
	const fakeClient = {
		getQueryData: (key: unknown) => store.get(keyOf(key)),
		setQueryData: (key: unknown, updater: unknown) => {
			const next =
				typeof updater === 'function'
					? (updater as (old: unknown) => unknown)(store.get(keyOf(key)))
					: updater;
			store.set(keyOf(key), next);
			return next;
		},
		removeQueries: (filters?: { queryKey?: unknown }) => {
			if (filters?.queryKey === undefined) {
				store.clear();
				return;
			}
			const prefix = keyOf(filters.queryKey).slice(0, -1);
			for (const k of [...store.keys()]) {
				if (k.startsWith(prefix)) store.delete(k);
			}
		},
		invalidateQueries: vi.fn(async () => undefined),
		cancelQueries: vi.fn(async () => undefined),
		clear: () => store.clear(),
		ensureQueryData: async (opts: {
			queryKey: unknown;
			queryFn: (ctx: { queryKey: unknown; signal: AbortSignal }) => Promise<unknown>;
		}) => {
			const k = keyOf(opts.queryKey);
			if (!store.has(k)) {
				store.set(
					k,
					await opts.queryFn({
						queryKey: opts.queryKey,
						signal: new AbortController().signal
					})
				);
			}
			return store.get(k);
		}
	};
	return {
		queryClient: fakeClient,
		invalidateQueriesWithPersister: (...args: unknown[]) => h.invalidate(...args),
		setQueryDataWithPersister: async (key: unknown, updater: unknown): Promise<void> => {
			fakeClient.setQueryData(key, updater);
		}
	};
});

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
});
