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
	'$lib/features/library-management/queries/LibraryManagementPreviewTokens',
	async () => (await import('./libraryManagementTestFixtures')).previewTokensMock
);
vi.mock(
	'$lib/features/library-management/queries/LibraryManagementQueries.svelte',
	async () => (await import('./libraryManagementTestFixtures')).queriesMock
);
vi.mock(
	'$lib/features/library-management/queries/LibraryManagementMutations.svelte',
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
		await expect.element(page.getByRole('button', { name: 'Validate and save' })).toBeDisabled();

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
		const reference = profileDialog.getByRole('link', { name: 'Script reference' }).first();
		await expect.element(reference).toBeVisible();
		await profileDialog.getByText('Metadata fields').click();
		await expect.element(reference).toBeVisible();
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

		await profileDialog.getByText('Preservation', { exact: true }).click();
		await profileDialog.getByText('Post-write notifications').click();
		await expect
			.element(profileDialog.getByText('DroppedNeedle catalog updates immediately'))
			.toBeVisible();
		await expect
			.element(profileDialog.getByRole('checkbox', { name: /Refresh DroppedNeedle/ }))
			.not.toBeInTheDocument();
	});

	it('names the retention a save over unreadable settings would apply', async () => {
		h.settings = {
			...h.settings,
			data: { ...baseSettings(), settings_unreadable: true, recycle_retention_days: 365 }
		};
		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await expect
			.element(page.getByText('Bin files older than 365 days are deleted after you save.'))
			.toBeVisible();
	});

	it('surfaces a server-side activation dry run beside the save bar', async () => {
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
		await expect.element(page.getByRole('button', { name: 'Validate and save' })).toBeDisabled();

		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await expect.element(page.getByRole('button', { name: 'Validate and save' })).toBeEnabled();
	});

	it('advances the preset version only when the reset section is file organization', async () => {
		type Settings = ReturnType<typeof baseSettings>;
		type Profile = Settings['profiles'][number];
		const cases: {
			section: string;
			group: string;
			arrange: (settings: Settings, preset: Profile) => void;
			saved: Record<string, unknown>;
		}[] = [
			{
				section: 'Metadata',
				group: 'metadata',
				arrange: (settings, preset) => {
					settings.profiles[0].preset_version = 2;
					preset.metadata.fields[0].mode = 'replace';
				},
				saved: {
					preset_version: 2,
					metadata: expect.objectContaining({
						fields: [expect.objectContaining({ mode: 'replace' })]
					})
				}
			},
			{
				section: 'File organization',
				group: 'organization',
				arrange: (settings, preset) => {
					settings.profiles[0].artwork.embedded_maximum_size = 777;
					preset.artwork.embedded_maximum_size = 777;
					preset.organization.move_enabled = false;
				},
				saved: {
					preset_version: 3,
					metadata: expect.objectContaining({
						fields: [expect.objectContaining({ mode: 'fill_missing' })]
					}),
					artwork: expect.objectContaining({ embedded_maximum_size: 777 }),
					organization: expect.objectContaining({ move_enabled: false })
				}
			}
		];
		for (const { section, group, arrange, saved } of cases) {
			h.update.mockClear();
			const settings = baseSettings();
			const presetProfile = structuredClone(settings.profiles[0]);
			arrange(settings, presetProfile);
			presetProfile.preset_version = 3;
			h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
			h.presetDiff = {
				data: {
					profile_id: profileId,
					preset_origin: 'picard_style_organizer',
					preset_version: 3,
					differs: true,
					changed_groups: [group],
					version_upgrade_groups: ['organization'],
					preset_profile: presetProfile
				},
				isLoading: false,
				isError: false
			};

			const view = await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
			await page.getByRole('button', { name: 'Edit' }).click();
			const profileDialog = page.getByRole('dialog', { name: 'Picard-style Organizer' });
			await profileDialog.getByRole('button', { name: `Reset ${section}` }).click();
			await page
				.getByRole('dialog', { name: `Reset ${section}?` })
				.getByRole('button', { name: 'Reset section' })
				.click();
			await profileDialog.getByRole('button', { name: 'Save profile' }).click();

			expect(h.update, section).toHaveBeenCalledWith(
				expect.objectContaining({
					settings: expect.objectContaining({
						profiles: [expect.objectContaining(saved)]
					})
				})
			);
			view.unmount();
		}
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
});
