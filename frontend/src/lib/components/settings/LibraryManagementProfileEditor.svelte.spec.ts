import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import { baseSettings, resetLibraryManagementMocks } from './libraryManagementTestFixtures';

vi.mock(
	'$lib/queries/library-management/LibraryManagementQueries.svelte',
	async () => (await import('./libraryManagementTestFixtures')).queriesMock
);

import LibraryManagementProfileEditor from './LibraryManagementProfileEditor.svelte';

beforeEach(resetLibraryManagementMocks);

describe('LibraryManagementProfileEditor', () => {
	it('saves changes made in different sections into one profile', async () => {
		const settings = baseSettings();
		const onsave = vi.fn().mockResolvedValue(undefined);
		await render(LibraryManagementProfileEditor, {
			profile: settings.profiles[0],
			managedFields: settings.managed_fields,
			namingScripts: settings.naming_scripts,
			taggingScripts: settings.tagging_scripts,
			onclose: vi.fn(),
			onsave
		});
		const editor = page.getByRole('dialog', { name: 'Picard-style Organizer' });

		await editor.getByText('Lyrics and loudness').click();
		await editor.getByRole('checkbox', { name: /Fetch lyrics from LRCLIB/ }).click();

		await editor.getByText('Credits and genres').click();
		await editor.getByRole('combobox', { name: 'Artist credit style' }).selectOptions('canonical');

		await editor.getByText('Artwork', { exact: true }).click();
		await editor.getByText('Advanced artwork rules').click();
		await editor.getByRole('spinbutton', { name: 'Minimum width' }).fill('600');

		await editor.getByRole('button', { name: 'Save profile' }).click();

		expect(onsave).toHaveBeenCalledTimes(1);
		const [saved] = onsave.mock.calls[0];
		expect(saved.enrichment.lyrics.enabled).toBe(true);
		expect(saved.metadata.artist_credits.standardization).toBe('canonical');
		expect(saved.artwork.minimum_width).toBe(600);
	});
});
