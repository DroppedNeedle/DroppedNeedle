import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import { apiError, fakeApi, textReply, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import Toaster from '$lib/ui/Toaster.svelte';

import DropImportZone from './DropImportZone.svelte';

const MB = 1024 * 1024;

const zip = (megabytes: number) =>
	new File([new Uint8Array(megabytes * MB)], 'Album.zip', { type: 'application/zip' });

const job: RouteData<'POST /api/v3/import/uploads'> = {
	id: 'job-1',
	status: 'processing',
	created_at: 1_760_000_000,
	upload_name: 'Album.zip',
	user_id: 'u-1',
	user_name: 'Cora',
	error: null,
	items: []
};

async function renderZone() {
	await render(Toaster, {});
	await renderWithQuery(DropImportZone, {});
}

describe('DropImportZone', () => {
	it('shows how much has been sent while an upload is in flight', async () => {
		let answer: (value: typeof job) => void = () => undefined;
		fakeApi({
			'POST /api/v3/import/uploads': (request) => {
				request.sent?.(1 * MB);
				return new Promise((resolve) => (answer = resolve));
			}
		});
		await renderZone();
		await page.getByLabelText('Choose files to import').upload(zip(4));

		await expect.element(page.getByRole('progressbar', { name: 'Upload progress' })).toBeVisible();
		await expect.element(page.getByText('1 of 4 MB')).toBeVisible();

		answer(job);
		await expect.element(page.getByText('Import started - identifying your files.')).toBeVisible();
	});

	it('says why when the server or a proxy refuses the upload as too large', async () => {
		const api = fakeApi({
			'POST /api/v3/import/uploads': apiError(
				413,
				'PAYLOAD_TOO_LARGE',
				'Uploads are limited to 8 GB.'
			)
		});
		await renderZone();
		const files = page.getByLabelText('Choose files to import');
		await files.upload(zip(1));
		await expect.element(page.getByRole('alert')).toHaveTextContent('Uploads are limited to 8 GB.');

		api.use({
			'POST /api/v3/import/uploads': textReply(413, '<html>413 Request Entity Too Large</html>')
		});
		await files.upload(zip(1));
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('A proxy refused the upload as too large. Raise its upload limit.');
	});
});
