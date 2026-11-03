import { page, userEvent } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi, type FakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import DeleteAlbumModal from './DeleteAlbumModal.svelte';

const removeRoute = 'DELETE /api/v3/library/albums/{id}' as const;
const detailRoute = 'GET /api/v3/library/albums/{id}' as const;

type Detail = RouteData<typeof detailRoute>;

const seen: Detail = {
	id: 'local-1',
	title: 'Blue Lines',
	artist_id: 'artist-1',
	artist_name: 'Massive Attack',
	identity_state: 'local_only',
	is_compilation: false,
	track_count: 9,
	total_duration_seconds: 2700,
	total_size_bytes: 1024,
	format: 'flac',
	year: 1991,
	cover_available: false,
	art_missing: false,
	favorite: false,
	date_added: 1,
	row_revision: 3
} as Detail;

let api: FakeApi;

async function renderModal(ondeleted = vi.fn(), onclose = vi.fn()) {
	return {
		ondeleted,
		onclose,
		...(await renderWithQuery(DeleteAlbumModal, {
			albumTitle: 'Blue Lines',
			artistName: 'Massive Attack',
			musicbrainzId: 'rg-1',
			ondeleted,
			onclose
		}))
	};
}

function remove() {
	return page.getByRole('button', { name: 'Remove', exact: true }).click();
}

describe('DeleteAlbumModal', () => {
	beforeEach(() => {
		api = fakeApi({
			'GET /api/v3/library/albums/{id}/status': {
				in_library: true,
				album_id: 'local-1',
				track_count: 9,
				tracks: []
			},
			[detailRoute]: seen,
			[removeRoute]: { id: 'local-1', removed_track_ids: [] }
		});
	});

	it('describes only the selected album file deletion', async () => {
		await renderModal();

		await expect.element(page.getByRole('heading', { name: 'Remove Album' })).toBeVisible();
		await expect.element(page.getByText(/Blue Lines/)).toBeVisible();
		await expect.element(page.getByText(/go to the recycle bin/)).toBeVisible();
		await expect.element(page.getByText(/also remove/i)).not.toBeInTheDocument();
		await expect.element(page.getByText(/Checking artist impact/i)).not.toBeInTheDocument();
	});

	it('removes the local album at the revision its detail shows', async () => {
		const { ondeleted } = await renderModal();
		await vi.waitFor(() => expect(api.calls(detailRoute)).toHaveLength(1));

		await remove();

		await vi.waitFor(() => expect(ondeleted).toHaveBeenCalledOnce());
		const [call] = api.calls(removeRoute);
		expect(call.params.id).toBe('local-1');
		expect(call.query.get('expected_revision')).toBe('3');
		expect(call.query.get('expected_track_count')).toBe('9');
		expect(call.query.get('stop_wanted')).toBe('true');
	});

	it('lets the administrator keep the Wanted watch', async () => {
		await renderModal();
		const checkbox = page.getByRole('checkbox', { name: /Stop the Wanted watcher/i });
		await expect.element(checkbox).toBeChecked();
		await vi.waitFor(() => expect(api.calls(detailRoute)).toHaveLength(1));

		await checkbox.click();
		await remove();

		await vi.waitFor(() => expect(api.calls(removeRoute)).toHaveLength(1));
		expect(api.calls(removeRoute)[0].query.get('stop_wanted')).toBe('false');
	});

	it('a stale refusal says what changed, and the next try sends the new revision', async () => {
		const { ondeleted } = await renderModal();
		await vi.waitFor(() => expect(api.calls(detailRoute)).toHaveLength(1));
		api.use({
			[detailRoute]: { ...seen, title: 'Blue Lines (Remaster)', track_count: 10, row_revision: 4 },
			[removeRoute]: apiError(409, 'STALE_REVISION', 'This album changed after you opened it.')
		});

		await remove();

		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent(/It is now Blue Lines \(Remaster\) by Massive Attack, 10 tracks\./);
		expect(ondeleted).not.toHaveBeenCalled();

		api.use({ [removeRoute]: { id: 'local-1', removed_track_ids: [] } });
		await remove();

		await vi.waitFor(() => expect(ondeleted).toHaveBeenCalledOnce());
		expect(api.calls(removeRoute).map((call) => call.query.get('expected_revision'))).toEqual([
			'3',
			'4'
		]);
	});

	it('names the copy that goes when several are held', async () => {
		api.use({
			'GET /api/v3/library/albums/{id}/status': {
				in_library: true,
				album_id: 'local-1',
				track_count: 2,
				tracks: [
					{ album_id: 'local-1' },
					{ album_id: 'local-2' }
				] as RouteData<'GET /api/v3/library/albums/{id}/status'>['tracks']
			}
		});
		await renderModal();

		await expect.element(page.getByText(/You hold 2 copies\. This one goes:/)).toBeVisible();
		await expect.element(page.getByText(/FLAC, 9 tracks/)).toBeVisible();
	});

	it('a failed read reads again on the next click and removes nothing', async () => {
		api.use({ [detailRoute]: apiError(404, 'NOT_FOUND', 'Gone') });
		await renderModal();
		await vi.waitFor(() => expect(api.calls(detailRoute).length).toBeGreaterThan(0));
		await expect.element(page.getByRole('button', { name: 'Remove', exact: true })).toBeEnabled();
		api.use({ [detailRoute]: seen });

		await remove();

		await expect.element(page.getByText(/FLAC, 9 tracks/)).toBeVisible();
		expect(api.calls(removeRoute)).toHaveLength(0);
	});

	it('keeps the confirmation open when removal fails', async () => {
		const { ondeleted } = await renderModal();
		await vi.waitFor(() => expect(api.calls(detailRoute)).toHaveLength(1));
		api.use({ [removeRoute]: apiError(409, 'LIBRARY_BUSY', "Couldn't remove this album") });

		await remove();

		await expect.element(page.getByText("Couldn't remove this album")).toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Remove Album' })).toBeVisible();
		expect(ondeleted).not.toHaveBeenCalled();
	});

	it('closes with the keyboard without removing the album', async () => {
		const { onclose } = await renderModal();

		await userEvent.keyboard('{Escape}');

		await vi.waitFor(() => expect(onclose).toHaveBeenCalledOnce());
		expect(api.calls(removeRoute)).toHaveLength(0);
	});
});
