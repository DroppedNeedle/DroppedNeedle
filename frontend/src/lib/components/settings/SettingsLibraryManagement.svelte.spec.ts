import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import {
	h,
	profileId,
	baseSettings,
	roots,
	activeAssignmentSettings,
	resetLibraryManagementMocks
} from './libraryManagementTestFixtures';

vi.mock(
	'$lib/queries/library-management/LibraryManagementPreviewTokens',
	async () => (await import('./libraryManagementTestFixtures')).previewTokensMock
);
vi.mock(
	'$lib/queries/library-management/LibraryManagementQueries.svelte',
	async () => (await import('./libraryManagementTestFixtures')).queriesMock
);
vi.mock(
	'$lib/queries/library-management/LibraryManagementMutations.svelte',
	async () => (await import('./libraryManagementTestFixtures')).mutationsMock
);

import SettingsLibraryManagement from './SettingsLibraryManagement.svelte';

beforeEach(resetLibraryManagementMocks);

describe('SettingsLibraryManagement', () => {
	it('offers merge only where the field catalog does, and edits genre through the Genres section', async () => {
		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('button', { name: 'Edit' }).click();
		const profileDialog = page.getByRole('dialog', { name: 'Picard-style Organizer' });
		await profileDialog.getByText('Metadata fields').click();

		const title = profileDialog.getByRole('combobox', { name: 'Mode for Title' });
		await expect.element(title).toHaveValue('fill_missing');
		await expect.element(title.getByRole('option', { name: 'Merge' })).not.toBeInTheDocument();
		const label = profileDialog.getByRole('combobox', { name: 'Mode for Label' });
		await expect.element(label).toHaveValue('disabled');
		await expect.element(label.getByRole('option', { name: 'Merge' })).toBeInTheDocument();
		await expect
			.element(profileDialog.getByText('No source: kept, cleared, or set by a script'))
			.not.toBeInTheDocument();
		await expect.element(profileDialog.getByText('From MusicBrainz: Release labels')).toBeVisible();
		await expect
			.element(profileDialog.getByText("AcoustID lookup of the file's audio", { exact: true }))
			.toBeVisible();

		const genre = profileDialog.getByRole('combobox', { name: 'Mode for Genre' });
		await expect.element(genre).toHaveValue('replace');
		await expect
			.element(profileDialog.getByRole('checkbox', { name: /Clear Genre/ }))
			.not.toBeInTheDocument();
		await genre.selectOptions('merge');
		await label.selectOptions('merge');
		await profileDialog.getByRole('button', { name: 'Save profile' }).click();
		expect(h.update).toHaveBeenCalledWith(
			expect.objectContaining({
				settings: expect.objectContaining({
					profiles: [
						expect.objectContaining({
							genres: expect.objectContaining({ enabled: true, mode: 'merge' }),
							metadata: expect.objectContaining({
								fields: [
									expect.objectContaining({ field: 'title', mode: 'fill_missing' }),
									expect.objectContaining({ field: 'label', mode: 'merge' })
								]
							})
						})
					]
				})
			})
		);
	});

	it('turns genres off from the genre row', async () => {
		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('button', { name: 'Edit' }).click();
		const profileDialog = page.getByRole('dialog', { name: 'Picard-style Organizer' });
		await profileDialog.getByText('Metadata fields').click();
		await profileDialog.getByRole('combobox', { name: 'Mode for Genre' }).selectOptions('disabled');
		await profileDialog.getByRole('button', { name: 'Save profile' }).click();
		expect(h.update).toHaveBeenCalledWith(
			expect.objectContaining({
				settings: expect.objectContaining({
					profiles: [
						expect.objectContaining({
							genres: expect.objectContaining({ enabled: false }),
							metadata: expect.objectContaining({
								fields: [expect.objectContaining({ field: 'title' })]
							})
						})
					]
				})
			})
		);
	});

	it('starts off everywhere and preserves subordinate profile values while a master toggle is off', async () => {
		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await expect.element(page.getByText('Off everywhere')).toBeVisible();
		await expect.element(page.getByText('Scanning: Automatic identification')).toBeVisible();
		await expect.element(page.getByText('Library default', { exact: true })).toBeVisible();
		await expect
			.element(page.getByRole('radio', { name: 'Make Picard-style Organizer the library default' }))
			.toBeChecked();
		await expect
			.element(page.getByRole('button', { name: 'Choose Management profile for Archive' }))
			.toHaveTextContent(/Inherited from library default.*Picard-style Organizer/);
		await expect
			.element(page.getByRole('combobox', { name: 'Management profile for Archive' }))
			.not.toBeInTheDocument();
		await expect.element(page.getByText('Configuration saved')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Saved' })).toBeDisabled();

		await page.getByRole('button', { name: 'Edit' }).click();
		const profileDialog = page.getByRole('dialog', { name: 'Picard-style Organizer' });
		await expect
			.element(profileDialog.getByRole('heading', { name: 'Picard-style Organizer' }))
			.toHaveFocus();
		await expect.element(profileDialog.getByText('Customized from preset')).toBeVisible();
		await expect
			.element(profileDialog.getByText('Changed sections: metadata, file organization'))
			.toBeVisible();
		await expect.element(profileDialog.getByText('1 managed field')).toBeVisible();
		await expect.element(profileDialog.getByText('Lyrics off')).toBeVisible();
		await expect.element(profileDialog.getByText('ReplayGain off')).toBeVisible();
		await expect
			.element(profileDialog.getByRole('button', { name: 'Save profile' }))
			.toBeDisabled();
		await expect.element(profileDialog.getByText('Manage metadata tags')).not.toBeVisible();

		await profileDialog.getByText('File naming and organization').click();
		await profileDialog.getByText('Language reference').click();
		await expect.element(profileDialog.getByText('Alpha/Management Track.flac')).toBeVisible();
		await expect
			.element(profileDialog.getByText(/Run a dry preview for real file results/))
			.toBeVisible();
		await profileDialog.getByText('Metadata fields').click();
		await expect.element(profileDialog.getByText('Alpha/Management Track.flac')).toBeVisible();
		const metadataToggle = profileDialog.getByRole('checkbox', { name: /Manage metadata tags/ });
		await metadataToggle.click();
		await expect.element(profileDialog.getByText('Saved choices retained')).toBeVisible();
		await expect
			.element(profileDialog.getByRole('combobox', { name: 'Mode for title' }))
			.not.toBeInTheDocument();
		await metadataToggle.click();
		await expect.element(profileDialog.getByText('1 managed field')).toBeVisible();
		await expect
			.element(profileDialog.getByRole('combobox', { name: 'Mode for title' }))
			.toHaveValue('fill_missing');

		await profileDialog.getByText('Lyrics and loudness').click();
		const lyricsToggle = profileDialog.getByRole('checkbox', {
			name: /Fetch lyrics from LRCLIB/
		});
		const plainLyrics = profileDialog.getByRole('checkbox', { name: /Write plain lyrics/ });
		await expect.element(plainLyrics).toBeDisabled();
		await lyricsToggle.click();
		await expect.element(plainLyrics).toBeEnabled();
		await expect.element(plainLyrics).toBeChecked();
		await profileDialog.getByText('Advanced lyrics behavior').click();
		const preserveLyrics = profileDialog.getByRole('checkbox', {
			name: /Preserve existing lyrics/
		});
		await expect.element(preserveLyrics).toBeEnabled();
		await expect.element(preserveLyrics).not.toBeChecked();
		await preserveLyrics.click();
		await expect.element(preserveLyrics).toBeChecked();
		await expect.element(profileDialog.getByText('Lyrics · synced + plain')).toBeVisible();

		const replayGainToggle = profileDialog.getByRole('checkbox', {
			name: /Manage ReplayGain/
		});
		const replayGainMode = profileDialog.getByRole('combobox', {
			name: 'Existing ReplayGain values'
		});
		await expect.element(replayGainMode).toBeDisabled();
		await replayGainToggle.click();
		await expect.element(replayGainMode).toBeEnabled();
		await expect.element(replayGainMode).toHaveValue('preserve');
		await expect.element(profileDialog.getByText('ReplayGain · preserve')).toBeVisible();

		await profileDialog.getByText('Lyrics and loudness').click();
		await expect.element(profileDialog.getByText('Preserve existing lyrics')).not.toBeVisible();
		await profileDialog.getByText('Lyrics and loudness').click();
		await expect
			.element(profileDialog.getByRole('checkbox', { name: /Preserve existing lyrics/ }))
			.toBeChecked();

		await profileDialog.getByText('Preservation and format safety').click();
		await profileDialog.getByText('Post-write notifications').click();
		await expect
			.element(profileDialog.getByText('DroppedNeedle catalog updates immediately'))
			.toBeVisible();
		await expect
			.element(profileDialog.getByRole('checkbox', { name: /Refresh DroppedNeedle/ }))
			.not.toBeInTheDocument();
	});

	it('surfaces a server-side activation dry run instead of an inert save action', async () => {
		h.operations = {
			data: {
				pages: [
					{
						items: [
							{
								operation: { id: 'preview-remote', state: 'running' },
								profile_name: 'Picard-style Organizer + Lyrics',
								target_root_id: 'root-1',
								activation_preview: true
							}
						]
					}
				]
			},
			isLoading: false,
			isError: false
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		await expect.element(page.getByText('Write-access dry run in progress')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Review dry run' }))
			.toHaveAttribute('href', '/library/management/previews/preview-remote');
		await expect
			.element(page.getByRole('button', { name: 'Validate and save' }))
			.not.toBeInTheDocument();

		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await expect.element(page.getByRole('button', { name: 'Validate and save' })).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Review dry run' }))
			.not.toBeInTheDocument();
	});

	it('does not advance the preset version when resetting metadata', async () => {
		const settings = baseSettings();
		settings.profiles[0].preset_version = 2;
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
		const presetProfile = structuredClone(settings.profiles[0]);
		presetProfile.preset_version = 3;
		presetProfile.metadata.fields[0].mode = 'replace';
		h.presetDiff = {
			data: {
				profile_id: profileId,
				preset_origin: 'picard_style_organizer',
				preset_version: 3,
				differs: true,
				changed_groups: ['metadata'],
				version_upgrade_groups: ['organization'],
				preset_profile: presetProfile
			},
			isLoading: false,
			isError: false
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('button', { name: 'Edit' }).click();
		const profileDialog = page.getByRole('dialog', { name: 'Picard-style Organizer' });
		await profileDialog.getByRole('button', { name: 'Reset Metadata' }).click();
		await page
			.getByRole('dialog', { name: 'Reset Metadata?' })
			.getByRole('button', { name: 'Reset section' })
			.click();
		await profileDialog.getByRole('button', { name: 'Save profile' }).click();

		expect(h.update).toHaveBeenCalledWith(
			expect.objectContaining({
				settings: expect.objectContaining({
					profiles: [
						expect.objectContaining({
							preset_version: 2,
							metadata: expect.objectContaining({
								fields: [expect.objectContaining({ mode: 'replace' })]
							})
						})
					]
				})
			})
		);
	});

	it('advances the preset version when resetting file organization only', async () => {
		const settings = baseSettings();
		settings.profiles[0].artwork.embedded_maximum_size = 777;
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
		const presetProfile = structuredClone(settings.profiles[0]);
		presetProfile.preset_version = 3;
		presetProfile.organization.move_enabled = false;
		h.presetDiff = {
			data: {
				profile_id: profileId,
				preset_origin: 'picard_style_organizer',
				preset_version: 3,
				differs: true,
				changed_groups: ['organization'],
				version_upgrade_groups: ['organization'],
				preset_profile: presetProfile
			},
			isLoading: false,
			isError: false
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('button', { name: 'Edit' }).click();
		const profileDialog = page.getByRole('dialog', { name: 'Picard-style Organizer' });
		await profileDialog.getByRole('button', { name: 'Reset File organization' }).click();
		await page
			.getByRole('dialog', { name: 'Reset File organization?' })
			.getByRole('button', { name: 'Reset section' })
			.click();
		await profileDialog.getByRole('button', { name: 'Save profile' }).click();

		expect(h.update).toHaveBeenCalledWith(
			expect.objectContaining({
				settings: expect.objectContaining({
					profiles: [
						expect.objectContaining({
							preset_version: 3,
							metadata: expect.objectContaining({
								fields: [expect.objectContaining({ mode: 'fill_missing' })]
							}),
							artwork: expect.objectContaining({ embedded_maximum_size: 777 }),
							organization: expect.objectContaining({ move_enabled: false })
						})
					]
				})
			})
		);
	});

	it('explains blocked storage without offering a dry run', async () => {
		const settings = activeAssignmentSettings();
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
		const refetch = vi.fn();
		h.health = {
			data: {
				stale_root_ids: [],
				blocked_root_ids: ['root-1'],
				blocked_reason: 'Library root Archive is not currently available.'
			},
			isLoading: false,
			isFetching: false,
			refetch
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		await expect.element(page.getByText('Paused: needs attention', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Automatic writes paused: needs attention')).toBeVisible();
		await expect
			.element(page.getByText(/Archive needs attention before automatic writes/))
			.toBeVisible();
		await expect
			.element(page.getByText('Library root Archive is not currently available.'))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Run a fresh dry run' }))
			.not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Recheck' }).click();
		expect(refetch).toHaveBeenCalled();
	});

	it('resets one preset section in the draft and confirms before discarding changes', async () => {
		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('button', { name: 'Edit' }).click();
		const profileDialog = page.getByRole('dialog', { name: 'Picard-style Organizer' });
		const resetButton = profileDialog.getByRole('button', { name: 'Reset Metadata' });
		await resetButton.click();

		const resetDialog = page.getByRole('dialog', { name: 'Reset Metadata?' });
		await expect
			.element(resetDialog.getByRole('heading', { name: 'Reset Metadata?' }))
			.toHaveFocus();
		await expect
			.element(resetDialog.getByText(/Review the values, then save the profile/))
			.toBeVisible();
		await resetDialog.getByRole('button', { name: 'Reset section' }).click();
		await profileDialog.getByText('Metadata fields').click();
		await expect
			.element(profileDialog.getByRole('combobox', { name: 'Mode for title' }))
			.toHaveValue('replace');
		await expect.element(resetButton).not.toBeInTheDocument();

		const cancelButton = profileDialog.getByRole('button', { name: 'Cancel' });
		await cancelButton.click();
		const discardDialog = page.getByRole('dialog', { name: 'Discard your changes?' });
		await expect
			.element(discardDialog.getByRole('heading', { name: 'Discard your changes?' }))
			.toHaveFocus();
		await discardDialog.getByRole('button', { name: 'Keep editing' }).click();
		await expect.element(cancelButton).toHaveFocus();
		await expect.element(profileDialog).toBeVisible();
	});
});
