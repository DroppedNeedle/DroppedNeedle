import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { components } from '$lib/api/v3/openapi';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import '../../../../../app.css';
import EditionNextSteps from './EditionNextSteps.svelte';

type EditionStatus = components['schemas']['EditionStatusView'];
type EditionChoice = components['schemas']['EditionChoiceView'];

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
	retag_files: [{ track_id: 'track-1', root_id: 'root-1', rel_path: '01 Lamplight.flac' }],
	status: status()
} as unknown as EditionChoice;

function show(edition: EditionStatus) {
	return renderWithQuery(EditionNextSteps, {
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
	});
}

beforeEach(() => {
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

describe('the Tags step of a new edition', () => {
	it('offers the tag write only in a folder the app writes, and says why not', async () => {
		fakeApi({});
		const kept = await show(status({ keeps_own_tags: true }));
		await expect.element(page.getByText('keeps its own tags')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Preview the new tags' }))
			.not.toBeInTheDocument();
		kept.unmount();

		await show(status());
		await expect.element(page.getByRole('button', { name: 'Preview the new tags' })).toBeVisible();
	});
});
