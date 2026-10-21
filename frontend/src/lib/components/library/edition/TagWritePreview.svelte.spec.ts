import { QueryClientProvider } from '@tanstack/svelte-query';
import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, onTestFinished, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { components } from '$lib/api/v3/openapi';
import { toAuthUser } from '$lib/queries/auth/types';
import { clearPersistedQueryCache } from '$lib/queries/IndexedDbPersister.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { apiError, fakeApi, type RouteData } from '$lib/test/api';

import '../../../../app.css';
import EditionNextSteps from './EditionNextSteps.svelte';

type EditionStatus = components['schemas']['EditionStatusView'];
type EditionChoice = components['schemas']['EditionChoiceView'];
type TagWriteJob = RouteData<'GET /api/v3/library/albums/{album_id}/edition/tags/{job_id}'>;

const album = { userId: 'user-1', localId: 'local-1', rgMbid: 'group-1' };

function status(overrides: Partial<EditionStatus> = {}): EditionStatus {
	return {
		album_id: 'local-1',
		candidates: [],
		edition_kind: 'release',
		files_by_itself: false,
		keeps_own_tags: false,
		identifying: false,
		identity_revision: 2,
		placing: null,
		reason: { code: 'SUPPORTED', message: 'The files fit this edition.', action: '' },
		release_group_mbid: 'group-1',
		release_mbid: 'release-remaster',
		state: 'chosen',
		undo_available: true,
		writing: false,
		...overrides
	} as EditionStatus;
}

const choice = {
	album_id: 'local-1',
	extra_track_ids: [],
	missing_titles: [],
	release_group_mbid: 'group-1',
	release_mbid: 'release-remaster',
	retag_files: ['01 Lamplight.flac', '02 Blue Hour.flac', '03 Night Shift.flac'].map(
		(rel_path) => ({ track_id: rel_path, root_id: 'root-1', rel_path })
	),
	status: status()
} as unknown as EditionChoice;

function job(overrides: Partial<TagWriteJob> = {}): TagWriteJob {
	return {
		job_id: 'write-1',
		step: 'ready',
		undo: false,
		undo_available: false,
		row_revision: 4,
		ready_to_apply: true,
		expired: false,
		stale_reasons: [],
		reason: null,
		files: [
			{
				track_id: 'track-1',
				path: 'Night Shift/01 Lamplight.flac',
				changes: [
					{ field: 'album', from: 'Night Shift', to: 'Night Shift (5th anniversary remaster)' },
					{ field: 'comment', from: null, to: 'Tagged by the folder' },
					{ field: 'musicbrainz_ids', from: null, to: '4 changed' }
				],
				removed_tags: [],
				artwork_changed: false,
				added_files: ['Night Shift/cover.jpg'],
				blocked: null
			},
			...['02 Blue Hour.flac', '03 Night Shift.flac'].map((name, i) => ({
				track_id: `track-${i + 2}`,
				path: `Night Shift/${name}`,
				changes: [],
				removed_tags: [],
				artwork_changed: false,
				added_files: [],
				blocked: null
			}))
		],
		...overrides
	};
}

/** The server's tag writes by job id, as the fake answers them. */
let jobs: Map<string, TagWriteJob>;

function serve() {
	return fakeApi({
		'POST /api/v3/library/albums/{album_id}/edition/tags': () => {
			jobs.set('write-1', job({ step: 'planning', files: [] }));
			return { job_id: 'write-1', preview_token: 'token-1', expires_at: 0 };
		},
		'GET /api/v3/library/albums/{album_id}/edition/tags/{job_id}': ({ params }) => {
			const found = jobs.get(params.job_id);
			if (!found) return apiError(404, 'NOT_FOUND');
			// Planning ends on the next read.
			if (found.step === 'planning') jobs.set(params.job_id, job({ job_id: found.job_id }));
			return found;
		},
		'POST /api/v3/library/albums/{album_id}/edition/tags/{job_id}/apply': ({ params }) => {
			const found = jobs.get(params.job_id) ?? job();
			const done = { ...found, step: 'done' as const, row_revision: found.row_revision + 2 };
			jobs.set(params.job_id, done);
			return { ...found, step: 'writing' as const };
		},
		'POST /api/v3/library/albums/{album_id}/edition/tags/{job_id}/undo': () => {
			jobs.set('undo-1', job({ job_id: 'undo-1', undo: true }));
			return { job_id: 'undo-1', preview_token: 'token-2', expires_at: 0 };
		}
	});
}

// The sheet runs on the app's own client: the edition queries refresh the
// album through it when a write finishes, and that must not reach the read
// of the write itself.
function show(edition: EditionStatus = status()) {
	onTestFinished(async () => {
		queryClient.clear();
		await clearPersistedQueryCache();
	});
	const props = {
		name: '5th anniversary remaster',
		status: edition,
		plan: null,
		choice,
		held: false,
		error: null,
		onundo: vi.fn(),
		album,
		group: '',
		gaps: { missing: 0, versions: 0 },
		sourceReady: true,
		tracks: null,
		onacquire: vi.fn()
	};
	return render(
		EditionNextSteps,
		{ props },
		{ wrapper: QueryClientProvider, wrapperProps: { client: queryClient } }
	);
}

beforeEach(() => {
	localStorage.clear();
	jobs = new Map();
	// A curator, not an admin: whoever wrote the tags may undo them.
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Tris',
			role: 'trusted',
			email: null,
			avatar_url: null,
			username: 'tris',
			username_display: 'Tris'
		})
	);
});

describe("writing the edition's tags in a folder the app does not write by itself", () => {
	it('previews each change once planned, and writes nothing until "Write tags"', async () => {
		const api = serve();
		await show();
		await page.getByRole('button', { name: 'Preview the new tags' }).click();

		const list = page.getByRole('list', { name: 'Tags that change' });
		const albumTag = list.getByRole('definition').filter({ hasText: 'Night Shift (5th' });
		await expect.element(albumTag.getByRole('deletion')).toHaveTextContent('Night Shift');
		await expect
			.element(albumTag.getByRole('insertion'))
			.toHaveTextContent('Night Shift (5th anniversary remaster)');
		await expect.element(list.getByText('Tagged by the folder')).toBeVisible();
		await expect.element(list.getByText('4 changed')).toBeVisible();
		await expect.element(list.getByText('cover.jpg')).toBeVisible();
		await expect.element(page.getByText('2 files already carry these tags.')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Write tags to 1 file' })).toBeVisible();
		expect(api.calls('POST /api/v3/library/albums/{album_id}/edition/tags')[0].body).toEqual({
			release_mbid: 'release-remaster',
			expected_identity_revision: 2
		});
		expect(api.calls('POST /api/v3/library/albums/{album_id}/edition/tags/{job_id}/apply')).toEqual(
			[]
		);
	});

	it('writes with the preview token, then the writer undoes it through its own undo job', async () => {
		const api = serve();
		await show();
		await page.getByRole('button', { name: 'Preview the new tags' }).click();
		await page.getByRole('button', { name: 'Write tags to 1 file' }).click();

		await expect.element(page.getByText('Tags written.')).toBeVisible();
		// A finished write is read once more at most, then never again.
		const reads = () =>
			api.calls('GET /api/v3/library/albums/{album_id}/edition/tags/{job_id}').length;
		const seen = reads();
		await new Promise((resolve) => setTimeout(resolve, 1_200));
		expect(reads()).toBeLessThanOrEqual(seen + 1);
		expect(
			api.calls('POST /api/v3/library/albums/{album_id}/edition/tags/{job_id}/apply')[0].body
		).toEqual({
			preview_token: 'token-1',
			expected_row_revision: 4,
			idempotency_key: 'apply-write-1'
		});

		await page.getByRole('button', { name: 'Undo', exact: true }).click();
		await expect.element(page.getByText('The old tags are back.')).toBeVisible();
		const undo = api.calls('POST /api/v3/library/albums/{album_id}/edition/tags/{job_id}/undo');
		expect(undo[0].params.job_id).toBe('write-1');
		expect(undo[0].body).toEqual({ expected_row_revision: 6, idempotency_key: 'undo-write-1' });
		const applies = api.calls('POST /api/v3/library/albums/{album_id}/edition/tags/{job_id}/apply');
		expect(applies[1].params.job_id).toBe('undo-1');
		expect(applies[1].body).toMatchObject({ preview_token: 'token-2' });
	});

	it("shows the server's reason when the tags cannot be previewed", async () => {
		serve().use({
			'POST /api/v3/library/albums/{album_id}/edition/tags': apiError(
				409,
				'EDITION_TAGS_HELD',
				'This album is left unmanaged, so its files keep their tags.',
				{ action: 'Choose "Manage it again" on the album page first.' }
			)
		});
		await show();
		await page.getByRole('button', { name: 'Preview the new tags' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent(
				'This album is left unmanaged, so its files keep their tags. Choose "Manage it again" on the album page first.'
			);
	});

	it('waits while the files are placed, saying why', async () => {
		serve();
		await show(status({ placing: { attempts: 0 } as EditionStatus['placing'] }));
		const button = page.getByRole('button', { name: 'Preview the new tags' });
		await expect.element(button).toBeDisabled();
		await expect
			.element(button)
			.toHaveAccessibleDescription(
				'Still matching this album to the edition you chose. Try again in a moment.'
			);
	});
});
