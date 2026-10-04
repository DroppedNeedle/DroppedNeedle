import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { RequestItem } from '$lib/queries/requests/types';

const h = vi.hoisted(() => ({
	isAdmin: false,
	reimportMutate: vi.fn()
}));

// Plain-object mocks only, so the browser worker never loads the real stores
// (mock factories that re-import the original module crash it).
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get isAdmin() {
			return h.isAdmin;
		},
		user: { id: 'user-a' }
	},
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	reimportDownloadV3: () => ({
		mutate: h.reimportMutate,
		isPending: false
	})
}));

import RequestCard from './RequestCard.svelte';

const albumId = '11111111-1111-1111-1111-111111111111';
const recordingId = '22222222-2222-2222-2222-222222222222';
const trackReleaseGroupId = '33333333-3333-3333-3333-333333333333';
const nowEpoch = () => Math.floor(Date.now() / 1000);

function makeItem(overrides: Partial<RequestItem> = {}): RequestItem {
	return {
		musicbrainz_id: albumId,
		artist_name: 'Radiohead',
		album_title: 'OK Computer',
		artist_mbid: null,
		year: 1997,
		requested_at: nowEpoch(),
		status: 'pending',
		request_kind: 'album',
		requester_count: 0,
		...overrides
	};
}

async function renderCard(
	overrides: Partial<RequestItem> = {},
	props: Record<string, unknown> = {}
) {
	return await render(RequestCard, {
		props: { item: makeItem(overrides), mode: 'active', ...props }
	} as unknown as Parameters<typeof render<typeof RequestCard>>[1]);
}

async function renderHistory(
	overrides: Partial<RequestItem> = {},
	props: Record<string, unknown> = {}
) {
	return await render(RequestCard, {
		props: { item: makeItem(overrides), mode: 'history', ...props }
	} as unknown as Parameters<typeof render<typeof RequestCard>>[1]);
}

describe('RequestCard.svelte', () => {
	beforeEach(() => {
		h.isAdmin = false;
		h.reimportMutate.mockReset();
	});

	it('keeps album requests displayed as albums', async () => {
		await renderCard();

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
		await renderCard({
			musicbrainz_id: recordingId,
			request_kind: 'track',
			track_title: 'Paranoid Android',
			album_title: 'OK Computer',
			track_release_group_mbid: trackReleaseGroupId
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

	it('uses the server cover URL when there is no artwork context', async () => {
		// a track without its release-group context has no MBID art to resolve,
		// so the card falls back to the server URL (a data URI here so the test
		// browser loads it instead of erroring the image away)
		const pixel = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';
		await renderCard({
			musicbrainz_id: recordingId,
			request_kind: 'track',
			track_title: 'Paranoid Android',
			album_title: 'OK Computer',
			cover_url: pixel
		});

		await expect.element(page.getByAltText('OK Computer')).toHaveAttribute('data-src', pixel);
	});

	it('shows progress, ETA, and sizes while downloading', async () => {
		await renderCard({
			status: 'downloading',
			progress: 40,
			eta: nowEpoch() + 601,
			size: 100 * 1024 * 1024,
			size_remaining: 60 * 1024 * 1024
		});

		await expect.element(page.getByText('40%', { exact: true })).toBeVisible();
		await expect.element(page.getByText('10 min', { exact: true })).toBeVisible();
		await expect.element(page.getByText('40.0 MB/100.0 MB')).toBeVisible();
	});

	it('shows quality and protocol while active', async () => {
		await renderCard({ status: 'downloading', quality: 'FLAC', protocol: 'soulseek' });

		await expect.element(page.getByText('FLAC', { exact: true })).toBeVisible();
		await expect.element(page.getByText('soulseek', { exact: true })).toBeVisible();
	});

	it('shows the failure line for failed active rows', async () => {
		await renderCard({ status: 'failed', error_message: 'mount gone' });

		await expect.element(page.getByText('mount gone', { exact: true })).toBeVisible();
	});

	it('expands status details when the task reports any', async () => {
		await renderCard({
			status: 'downloading',
			status_messages: [{ title: 'slskd', messages: ['queued remotely'] }]
		});

		await page.getByTitle('Show details').click();
		await expect.element(page.getByText('slskd', { exact: true })).toBeVisible();
		await expect.element(page.getByText('• queued remotely')).toBeVisible();
	});

	it('names co-requesters when others want the same row', async () => {
		await renderCard({ requester_count: 2 });
		await expect.element(page.getByText('+2 others')).toBeVisible();
	});

	it('passes the track kind through the cancel callback', async () => {
		const oncancel = vi.fn();
		await renderCard(
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
				status: 'failed',
				track_release_group_mbid: trackReleaseGroupId,
				completed_at: nowEpoch()
			},
			{ onretry, onclear }
		);
		await page.getByTitle('Retry request').click();
		await page.getByTitle('Clear from history').click();
		expect(onretry).toHaveBeenCalledWith(recordingId, 'track');
		expect(onclear).toHaveBeenCalledWith(recordingId, 'track');
	});

	it('offers batch selection only when selectable', async () => {
		const onselect = vi.fn();
		await renderCard({}, { selectable: true, selected: false, onselect });
		await page.getByRole('checkbox', { name: 'Select OK Computer' }).click();
		expect(onselect).toHaveBeenCalledWith(albumId, 'album', true);
	});

	it('offers admins the slskd reimport on failed rows with a linked task', async () => {
		h.isAdmin = true;
		const onreimported = vi.fn();
		await renderHistory(
			{ status: 'failed', can_reimport: true, task_id: 'task-1', completed_at: nowEpoch() },
			{ onreimported }
		);

		await page.getByRole('button', { name: 'Retry import from slskd' }).click();
		expect(h.reimportMutate).toHaveBeenCalledOnce();
		expect(h.reimportMutate.mock.calls[0]?.[0]).toEqual({
			id: 'task-1',
			release_group_mbid: albumId
		});
	});

	it('hides the reimport action without the flag, the task, or the role', async () => {
		h.isAdmin = true;
		await renderHistory({ status: 'failed', completed_at: nowEpoch() });
		expect(page.getByRole('button', { name: 'Retry import from slskd' }).elements()).toHaveLength(
			0
		);
	});

	it('gates the library removal on imported rows known to be shelved', async () => {
		h.isAdmin = true;
		await renderHistory({ status: 'imported', in_library: false, completed_at: nowEpoch() });
		expect(page.getByTitle('Remove from library').elements()).toHaveLength(0);
	});
});
