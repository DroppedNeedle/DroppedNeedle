import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

const saved = vi.hoisted(() => ({ blob: null as Blob | null, name: '' }));
vi.mock('$lib/utils/blobDownload', () => ({
	saveBlob: (blob: Blob, name: string) => {
		saved.blob = blob;
		saved.name = name;
	}
}));

import SupportBundleModal from './SupportBundleModal.svelte';

const bundle = {
	version: '3.0.0',
	commit_tag: 'dev',
	build_date: null,
	arch: 'x86_64',
	os: 'linux',
	kernel_release: '6.12.117',
	container: true,
	cpus: 4,
	memory_bytes: 8_000_000_000,
	folders: [],
	settings: [{ key: 'backups', state: 'saved' }],
	download_sources: [],
	loops: [],
	queues: [],
	degraded_services: [],
	database: {
		db_bytes: 1024,
		wal_bytes: null,
		cache_bytes: null,
		schema_version: 175,
		backup_count: 2,
		newest_nightly_backup_age_secs: null
	},
	counts: {
		users_by_role: { admin: 1 },
		artists: 2,
		albums: 3,
		tracks: 30,
		playlists: 0,
		requests_by_state: {},
		held_entries: 0
	},
	plugins: [],
	problems: [],
	problems_left_out: 0
} satisfies RouteData<'GET /api/v3/admin/support-bundle'>;

describe('the support bundle window', () => {
	it('shows the bundle and saves exactly what it shows', async () => {
		fakeApi({ 'GET /api/v3/admin/support-bundle': bundle });
		await renderWithQuery(SupportBundleModal, {});

		await page.getByRole('button', { name: 'Download support bundle' }).click();
		const shown = page.getByLabelText('Bundle JSON');
		await expect.element(shown).toHaveTextContent('"version": "3.0.0"');
		await page.getByRole('button', { name: 'Download', exact: true }).click();

		expect(saved.name).toMatch(/^droppedneedle-support-\d{4}-\d{2}-\d{2}\.json$/);
		expect(await saved.blob?.text()).toBe(JSON.stringify(bundle, null, 2));
	});
});
