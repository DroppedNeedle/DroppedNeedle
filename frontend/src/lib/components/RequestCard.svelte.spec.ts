import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { ActiveRequestItem, RequestHistoryItem } from '$lib/types';

const h = vi.hoisted(() => ({
	isAdmin: false,
	reimportMutate: vi.fn()
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	reimportDownload: () => ({
		mutate: h.reimportMutate,
		isPending: false
	})
}));

vi.mock('$lib/stores/authStore.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/stores/authStore.svelte')>()),
	authStore: {
		get isAdmin() {
			return h.isAdmin;
		},
		user: { id: 'user-a' }
	}
}));

import RequestCard from './RequestCard.svelte';

const albumId = '11111111-1111-1111-1111-111111111111';
const recordingId = '22222222-2222-2222-2222-222222222222';
const trackReleaseGroupId = '33333333-3333-3333-3333-333333333333';

function makeActive(overrides: Partial<ActiveRequestItem> = {}): ActiveRequestItem {
	return {
		musicbrainz_id: albumId,
		artist_name: 'Radiohead',
		album_title: 'OK Computer',
		artist_mbid: null,
		year: 1997,
		cover_url: null,
		requested_at: new Date().toISOString(),
		status: 'pending',
		request_kind: 'album',
		...overrides
	};
}

function makeHistory(overrides: Partial<RequestHistoryItem> = {}): RequestHistoryItem {
	return {
		musicbrainz_id: albumId,
		artist_name: 'Radiohead',
		album_title: 'OK Computer',
		artist_mbid: null,
		year: 1997,
		cover_url: null,
		requested_at: new Date().toISOString(),
		completed_at: new Date().toISOString(),
		status: 'failed',
		in_library: false,
		request_kind: 'album',
		...overrides
	};
}

async function renderActive(
	overrides: Partial<ActiveRequestItem> = {},
	props: Record<string, unknown> = {}
) {
	return await render(RequestCard, {
		props: { item: makeActive(overrides), mode: 'active', ...props }
	} as unknown as Parameters<typeof render<typeof RequestCard>>[1]);
}

async function renderHistory(
	overrides: Partial<RequestHistoryItem> = {},
	props: Record<string, unknown> = {}
) {
	return await render(RequestCard, {
		props: { item: makeHistory(overrides), mode: 'history', ...props }
	} as unknown as Parameters<typeof render<typeof RequestCard>>[1]);
}

describe('RequestCard.svelte', () => {
	beforeEach(() => {
		h.isAdmin = false;
		h.reimportMutate.mockReset();
	});

	it('keeps album requests displayed as albums', async () => {
		await renderActive();

		await expect.element(page.getByText('OK Computer', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Track', { exact: true })).not.toBeInTheDocument();
		await expect
			.element(page.getByAltText('OK Computer'))
			.toHaveAttribute('data-src', `/api/v1/covers/release-group/${albumId}?size=250`);
		await expect
			.element(page.getByRole('link', { name: 'Open OK Computer' }))
			.toHaveAttribute('href', `/album/${albumId}`);
	});

	it('shows a track title, album context, label, and release-group artwork', async () => {
		await renderActive({
			musicbrainz_id: recordingId,
			request_kind: 'track',
			track_title: 'Paranoid Android',
			album_title: 'OK Computer',
			track_release_group_mbid: trackReleaseGroupId,
			cover_url: '/api/v1/covers/release-group/cover-from-server?size=500'
		});

		await expect.element(page.getByText('Paranoid Android', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Track', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Album: OK Computer', { exact: true })).toBeVisible();
		await expect
			.element(page.getByAltText('OK Computer'))
			.toHaveAttribute('data-src', `/api/v1/covers/release-group/${trackReleaseGroupId}?size=250`);
		await expect
			.element(page.getByRole('link', { name: 'Open album context for OK Computer' }))
			.toHaveAttribute('href', `/album/${trackReleaseGroupId}`);
	});

	it('passes the track kind through the cancel callback', async () => {
		const oncancel = vi.fn();
		await renderActive(
			{
				musicbrainz_id: recordingId,
				request_kind: 'track',
				track_title: 'Paranoid Android',
				status: 'downloading',
				track_release_group_mbid: trackReleaseGroupId
			},
			{ oncancel }
		);
		await page.getByTitle('Cancel download').click();
		await page.getByRole('button', { name: 'Yes' }).click();
		expect(oncancel).toHaveBeenCalledWith(recordingId, 'track');
	});

	it('passes the track kind through retry and clear callbacks', async () => {
		const onretry = vi.fn();
		const onclear = vi.fn();
		await renderHistory(
			{
				musicbrainz_id: recordingId,
				request_kind: 'track',
				track_title: 'Paranoid Android',
				track_release_group_mbid: trackReleaseGroupId
			},
			{ onretry, onclear }
		);
		await page.getByTitle('Retry request').click();
		await page.getByTitle('Clear from history').click();
		expect(onretry).toHaveBeenCalledWith(recordingId, 'track');
		expect(onclear).toHaveBeenCalledWith(recordingId, 'track');
	});
});
