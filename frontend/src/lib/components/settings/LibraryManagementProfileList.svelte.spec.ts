import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { LibraryManagementSettingsResponse } from '$lib/queries/library-management/types';
import {
	h,
	baseSettings,
	roots,
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

describe('LibraryManagementProfileList', () => {
	it('reviews warnings and scripts before importing an inert custom profile', async () => {
		const settings = baseSettings();
		const previewProfile = {
			...structuredClone(settings.profiles[0]),
			id: 'a84ef040-aa7b-5982-837e-dc8c5ff68e9a',
			name: 'Picard-style Organizer (imported)',
			preset_origin: null,
			preset_version: null,
			revision: 'preview-profile'
		};
		const previewScript = {
			...structuredClone(settings.naming_scripts[0]),
			id: '17c66766-80bb-5af0-b6fd-40f63113b565',
			name: 'Picard-style: single disc (imported)',
			source: '{albumartist}/{album}/{track:02d} - {title}.{ext}',
			preset_origin: null,
			preset_version: null,
			revision: 'preview-script'
		};
		h.previewProfileImport.mockResolvedValue({
			profile: previewProfile,
			naming_scripts: [previewScript],
			tagging_scripts: [],
			aspects: ['Metadata tags', 'Artwork', 'Move files'],
			warnings: [
				{
					code: 'remove_sources',
					severity: 'danger',
					title: 'Removes verified move sources',
					message: 'Source files are removed after their managed moves are confirmed.'
				}
			],
			bundle_hash: 'b'.repeat(64),
			settings_revision: 'reviewed-settings-1'
		});
		const importedProfile = {
			...previewProfile,
			id: '98156edf-bf24-47e4-af67-1545f0d830d9',
			name: 'Shared organizer',
			revision: 'imported-profile'
		};
		const importedScript = {
			...previewScript,
			id: '8a664f88-3570-4ac8-9508-30ce8dc930e1',
			revision: 'imported-script'
		};
		h.importProfile.mockResolvedValue({
			profile: importedProfile,
			naming_scripts: [importedScript],
			tagging_scripts: [],
			settings_revision: 'settings-2'
		});

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('button', { name: 'Import profile' }).click();
		const dialog = page.getByRole('dialog', { name: 'Import profile' });
		await expect.element(dialog.getByRole('heading', { name: 'Import profile' })).toHaveFocus();
		await expect.element(dialog.getByText('Choose or drop a .dnprofile file')).toBeVisible();
		await dialog
			.getByLabelText('Profile file')
			.upload(new File(['{"format":"droppedneedle-library-profile"}'], 'shared.dnprofile'));
		await expect.element(dialog.getByText('shared.dnprofile')).toBeVisible();
		await dialog
			.getByLabelText('Profile file')
			.upload(new File(['x'.repeat(1_048_577)], 'oversized.dnprofile'));
		await expect.element(dialog.getByText('Profile files must be 1 MiB or smaller.')).toBeVisible();
		await expect.element(dialog.getByRole('button', { name: 'Review profile' })).toBeDisabled();
		await dialog.getByRole('textbox', { name: 'Or paste a text code' }).fill('DNLP1:shared');
		await expect.element(dialog.getByText('shared.dnprofile')).not.toBeInTheDocument();
		await dialog.getByRole('button', { name: 'Review profile' }).click();

		expect(h.previewProfileImport).toHaveBeenCalledWith({
			content: 'DNLP1:shared',
			expected_settings_revision: 'settings-1'
		});
		await expect.element(dialog.getByText('Custom and inactive')).toBeVisible();
		await expect.element(dialog.getByText('Removes verified move sources')).toBeVisible();
		await dialog.getByText('Picard-style: single disc (imported)').click();
		await expect.element(dialog.getByText(previewScript.source)).toBeVisible();
		await dialog.getByRole('textbox', { name: 'Imported profile name' }).fill('Shared organizer');
		await dialog.getByRole('button', { name: 'Import custom profile' }).click();

		expect(h.importProfile).toHaveBeenCalledWith({
			content: 'DNLP1:shared',
			reviewed_bundle_hash: 'b'.repeat(64),
			name: 'Shared organizer',
			expected_settings_revision: 'reviewed-settings-1'
		});
		const profileDialog = page.getByRole('dialog', { name: 'Shared organizer' });
		await expect
			.element(profileDialog.getByRole('heading', { name: 'Shared organizer' }))
			.toHaveFocus();
	});

	it.each([
		{
			name: 'Only custom profile',
			reason: 'Only remaining profile',
			prepare(settings: LibraryManagementSettingsResponse) {
				settings.profiles[0].name = 'Only custom profile';
				settings.profiles[0].preset_origin = null;
				settings.profiles[0].preset_version = null;
			}
		},
		{
			name: 'Default custom profile',
			reason: 'Library default',
			prepare(settings: LibraryManagementSettingsResponse) {
				const profile = structuredClone(settings.profiles[0]);
				profile.id = '1c56cd00-4f7d-42ee-97df-2710110a31d2';
				profile.name = 'Default custom profile';
				profile.preset_origin = null;
				profile.preset_version = null;
				settings.profiles.push(profile);
				settings.default_profile_id = profile.id;
			}
		},
		{
			name: 'Assigned custom profile',
			reason: 'Assigned to root',
			prepare(settings: LibraryManagementSettingsResponse) {
				const profile = structuredClone(settings.profiles[0]);
				profile.id = '94bf55a3-b553-4cf5-b18c-671194f67783';
				profile.name = 'Assigned custom profile';
				profile.preset_origin = null;
				profile.preset_version = null;
				settings.profiles.push(profile);
				settings.root_assignments = [
					{
						root_id: 'root-1',
						profile_id: profile.id,
						overrides: null,
						enabled: false,
						automatic_acquisitions: false,
						automatic_drop_imports: false,
						automatic_scan_discovered: false,
						automatic_custom_editions: false,
						activation_profile_revision: null,
						activation_naming_policy_revision: null,
						activation_policy_revision: null,
						activation_settings_revision: null,
						activation_preview_token: null,
						activation_preview_hash: null,
						activation_confirmed_at: null,
						activation_confirmed_by: null
					}
				];
			}
		}
	])('explains why $name cannot be deleted', async ({ name, reason, prepare }) => {
		const settings = baseSettings();
		prepare(settings);
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		const control = page.getByRole('button', { name: `Cannot delete ${name}: ${reason}` });
		await expect.element(control).toBeDisabled();
		await expect.element(control).toHaveTextContent(reason);
	});

	it('deletes the final custom profile in the scrollable card list', async () => {
		const settings = baseSettings();
		const profileIds = [
			'1c56cd00-4f7d-42ee-97df-2710110a31d2',
			'94bf55a3-b553-4cf5-b18c-671194f67783',
			'32607bf8-19a4-44d0-9757-d93b26de4052',
			'a945fc94-c072-4a0c-991d-e0cc4db5bd54'
		];
		const copies = profileIds.map((id, index) => ({
			...structuredClone(settings.profiles[0]),
			id,
			name: `Profile ${index + 1}`,
			preset_origin: null,
			preset_version: null,
			revision: `profile-${index + 2}`
		}));
		settings.profiles.push(...copies);
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
		h.copy.mockResolvedValue({ profile: copies[3], settings_revision: 'settings-2' });
		h.deleteProfile.mockResolvedValue({
			...settings,
			profiles: settings.profiles.filter((profile) => profile.id !== copies[3].id),
			settings_revision: 'settings-3'
		});

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		const profileRegion = page.getByRole('region', {
			name: 'Saved organization profiles'
		});
		await expect.element(profileRegion).toHaveAttribute('tabindex', '0');
		expect(profileRegion.getByRole('button', { name: /^Delete Profile/ }).all()).toHaveLength(4);

		await page.getByRole('textbox', { name: 'New profile name' }).fill('Profile 4');
		await page.getByRole('button', { name: 'Create copy' }).click();
		expect(profileRegion.getByRole('button', { name: 'Delete Profile 4' }).all()).toHaveLength(1);

		const profileDialog = page.getByRole('dialog', { name: 'Profile 4', exact: true });
		await profileDialog.getByRole('button', { name: 'Cancel' }).click();
		await profileRegion.getByRole('button', { name: 'Delete Profile 4' }).click();
		const deleteDialog = page.getByRole('dialog', { name: 'Delete Profile 4?' });
		await expect
			.element(deleteDialog.getByRole('heading', { name: 'Delete Profile 4?' }))
			.toHaveFocus();
		await deleteDialog.getByRole('button', { name: 'Delete profile', exact: true }).click();

		expect(h.deleteProfile).toHaveBeenCalledWith({
			profileId: copies[3].id,
			request: { expected_settings_revision: 'settings-2' }
		});
		await expect
			.element(profileRegion.getByRole('button', { name: 'Delete Profile 4' }))
			.not.toBeInTheDocument();
	});
});
