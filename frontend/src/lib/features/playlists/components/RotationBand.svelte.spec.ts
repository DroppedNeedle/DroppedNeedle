import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type FakeRequest, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

const toast = vi.hoisted(() => vi.fn());
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: toast } }));

import RotationBand from './RotationBand.svelte';
import type { RotationInfo } from './rotationIdeas';

type Summary = RouteData<'PUT /api/v3/playlists/{playlist_id}/rotation'>;

const NEXT = 1_795_996_800;

const LOSSLESS: RotationInfo = {
	idea: 'lossless',
	decade: null,
	pinned: false,
	always_on: false,
	next_change_at: NEXT
};

function summary(pinned: boolean): Summary {
	return {
		id: 'pl-1',
		name: 'Lossless',
		track_count: 50,
		total_duration: null,
		cover_urls: [],
		custom_cover_url: null,
		source_ref: null,
		created_at: 1,
		updated_at: 1,
		is_public: false,
		is_owner: true,
		is_shared: false,
		can_edit: false,
		can_delete: true,
		owner_name: 'Owner',
		is_redacted: false,
		kind: 'rotation',
		rotation: { ...LOSSLESS, pinned }
	};
}

beforeEach(() => {
	localStorage.clear();
	toast.mockClear();
	authStore.setUser(
		toAuthUser({
			id: 'owner-1',
			display_name: 'Owner',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'owner',
			username_display: 'Owner'
		})
	);
});

describe('the rotation band', () => {
	it('keeps a list on rotation with the switch', async () => {
		const api = fakeApi({
			'PUT /api/v3/playlists/{playlist_id}/rotation': (request: FakeRequest) =>
				summary((request.body as { pinned: boolean }).pinned)
		});
		await renderWithQuery(RotationBand, { playlistId: 'pl-1', rotation: LOSSLESS });

		await expect
			.element(page.getByText('FLAC, WAV and ALAC files, a new 50 each week.'))
			.toBeVisible();
		await page.getByRole('checkbox', { name: 'Keep on rotation' }).click();
		await expect
			.poll(() => api.calls('PUT /api/v3/playlists/{playlist_id}/rotation')[0]?.body)
			.toEqual({ pinned: true });
		await expect.element(page.getByRole('checkbox', { name: 'Keep on rotation' })).toBeChecked();
		await expect.element(page.getByRole('button', { name: 'Save a copy' })).toBeVisible();
	});

	it('offers no switch on an always-on list', async () => {
		fakeApi({});
		await renderWithQuery(RotationBand, {
			playlistId: 'pl-2',
			rotation: { ...LOSSLESS, idea: 'on_repeat', always_on: true }
		});

		await expect.element(page.getByText('What you played most in the last 30 days.')).toBeVisible();
		await expect
			.element(page.getByRole('checkbox', { name: 'Keep on rotation' }))
			.not.toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Save a copy' })).toBeVisible();
	});
});
