import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { LibraryManagementSettingsResponse } from '$lib/queries/library-management/types';

const h = vi.hoisted(() => ({
	settings: { data: {}, isLoading: false, isError: false, refetch: vi.fn() } as Record<
		string,
		unknown
	>,
	presetDiff: { data: null, isLoading: false, isError: false } as Record<string, unknown>,
	activation: { data: null, isLoading: false, refetch: vi.fn() } as Record<string, unknown>,
	validate: vi.fn(),
	impact: vi.fn(),
	update: vi.fn(),
	copy: vi.fn(),
	deleteProfile: vi.fn(),
	createActivation: vi.fn(),
	confirmActivation: vi.fn(),
	createActivationPending: false,
	confirmActivationPending: false,
	purgeImpact: vi.fn(),
	purge: vi.fn(),
	purgeData: null as Record<string, unknown> | null,
	remember: vi.fn()
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, user: { id: 'admin-1' } }
}));
vi.mock('$lib/queries/library-management/LibraryManagementPreviewTokens', () => ({
	rememberLibraryManagementPreviewToken: (...args: unknown[]) => h.remember(...args)
}));
vi.mock('$lib/queries/library-management/LibraryManagementQueries.svelte', () => ({
	getLibraryManagementSettingsQuery: () => h.settings,
	getLibraryManagementActivationPreviewQuery: () => h.activation,
	getLibraryManagementPresetDiffQuery: () => h.presetDiff
}));
vi.mock('$lib/queries/library-management/LibraryManagementMutations.svelte', () => ({
	updateLibraryManagementSettingsMutation: () => ({ mutateAsync: h.update, isPending: false }),
	validateLibraryManagementSettingsMutation: () => ({ mutateAsync: h.validate, isPending: false }),
	previewLibraryManagementSettingsImpactMutation: () => ({
		mutateAsync: h.impact,
		isPending: false
	}),
	copyLibraryManagementProfileMutation: () => ({ mutateAsync: h.copy, isPending: false }),
	deleteLibraryManagementProfileMutation: () => ({
		mutateAsync: h.deleteProfile,
		isPending: false
	}),
	createLibraryManagementActivationPreviewMutation: () => ({
		mutateAsync: h.createActivation,
		get isPending() {
			return h.createActivationPending;
		}
	}),
	confirmLibraryManagementActivationMutation: () => ({
		mutateAsync: h.confirmActivation,
		get isPending() {
			return h.confirmActivationPending;
		}
	}),
	previewLibraryManagementBaselinePurgeMutation: () => ({
		mutateAsync: h.purgeImpact,
		isPending: false,
		get data() {
			return h.purgeData;
		}
	}),
	purgeLibraryManagementBaselinesMutation: () => ({ mutateAsync: h.purge, isPending: false })
}));

import SettingsLibraryManagement from './SettingsLibraryManagement.svelte';

const profileId = 'c2741223-da7c-5231-bcf5-7cead27b07d9';
const namingScriptId = 'f66f6409-ba0c-5b9a-9258-8fb91eefcb0b';

function baseSettings(): LibraryManagementSettingsResponse {
	return {
		schema_version: 1,
		profiles: [
			{
				id: profileId,
				name: 'Picard-style Organizer',
				description: 'Canonical tags, artwork, and same-root organization.',
				preset_origin: 'picard_style_organizer',
				preset_version: 1,
				revision: 'profile-1',
				metadata: {
					enabled: true,
					fields: [{ field: 'title', mode: 'merge', clear_when_canonical_missing: false }],
					artist_credits: {
						standardization: 'credited',
						translate_names: false,
						preferred_locales: []
					},
					relationships: { enabled: true, types: ['composer', 'performer'] },
					tagging_script_ids: [],
					preserve_fields: [],
					scrub_unmanaged_tags: false,
					preserve_embedded_art_during_scrub: true,
					format_compatibility: {
						id3_version: '2.4',
						id3v23_join_delimiter: '; ',
						id3_text_encoding: 'utf8',
						remove_id3_from_flac: false,
						mp3_apev2_policy: 'preserve',
						raw_aac_tag_policy: 'save_apev2',
						wav_tag_policy: 'id3',
						constrained_genres_primary_only: false
					}
				},
				genres: {
					enabled: true,
					mode: 'replace',
					sources: ['musicbrainz', 'listenbrainz'],
					maximum_count: 5,
					musicbrainz_minimum_count: 1,
					listenbrainz_minimum_count: 1,
					lastfm_minimum_weight: 10,
					listenbrainz_curated_only: true,
					lastfm_whitelist_only: true,
					canonicalize: true,
					maximum_ancestry_depth: 4,
					allowlist: [],
					denylist: [],
					aliases: [],
					preferred_casing: [],
					write_primary_only_for_constrained_formats: false
				},
				artwork: {
					embedded_enabled: true,
					external_enabled: true,
					providers: ['cover_art_archive_release', 'local_files'],
					approved_only: true,
					download_size: 'full',
					local_file_patterns: ['cover.jpg'],
					image_types: ['front'],
					minimum_width: 0,
					minimum_height: 0,
					embedded_maximum_size: 1200,
					embedded_format: 'jpeg',
					external_maximum_size: 0,
					external_format: 'original',
					embedded_front_only: true,
					external_front_only: true,
					never_replace_with_smaller: true,
					preserve_existing_types: [],
					external_naming_script_id: null,
					overwrite_external_files: false
				},
				organization: {
					rename_enabled: true,
					move_enabled: true,
					naming_script_id: namingScriptId,
					compatibility: {
						windows_compatible: true,
						replace_non_ascii: false,
						replace_spaces_with_underscores: false,
						separator_replacement: '_',
						maximum_component_length: 240,
						maximum_path_length: 4096,
						unicode_normalization: 'NFC',
						extension_case: 'preserve',
						windows_legacy_path_limit: false
					},
					move_sidecars: true,
					sidecar_patterns: ['*.cue'],
					source_cleanup: 'remove_after_confirmed_move',
					remove_empty_directories: true
				},
				file_behavior: {
					preserve_timestamps: true,
					preserve_permissions: true,
					strict_capability_gate: true,
					reject_symlinks: true,
					validate_written_metadata: true,
					validate_technical_audio: true
				},
				enrichment: {
					lyrics: {
						enabled: false,
						provider: 'lrclib',
						write_plain: true,
						write_synced: true,
						required: false
					},
					replaygain: { enabled: false, mode: 'preserve', album_aware: true, required: false }
				},
				notification: { refresh_droppedneedle: true, refresh_external_servers: false }
			}
		],
		default_profile_id: profileId,
		root_assignments: [],
		naming_scripts: [
			{
				id: namingScriptId,
				name: 'Picard-style folders',
				source: '{albumartist}/{album} ({year})/{track:02} {title}.{ext}',
				revision: 'script-1',
				preset_origin: 'picard_style_organizer',
				preset_version: 1
			}
		],
		tagging_scripts: [],
		undo_retention_days: 90,
		preview_retention_hours: 24,
		recycle_bin_path: '',
		external_refresh: {
			enabled: false,
			plex_enabled: false,
			jellyfin_enabled: false,
			navidrome_enabled: false,
			retry_attempts: 3,
			retry_delay_seconds: 30
		},
		settings_revision: 'settings-1'
	};
}

const roots = [
	{
		id: 'root-1',
		path: '/music/archive',
		label: 'Archive',
		policy: 'automatic' as const,
		rules: []
	}
];

beforeEach(() => {
	vi.clearAllMocks();
	h.createActivationPending = false;
	h.confirmActivationPending = false;
	h.purgeData = null;
	const settings = baseSettings();
	const presetProfile = structuredClone(settings.profiles[0]);
	presetProfile.metadata.fields[0].mode = 'replace';
	presetProfile.organization.move_enabled = false;
	h.presetDiff = {
		data: {
			profile_id: profileId,
			preset_origin: 'picard_style_organizer',
			preset_version: 1,
			differs: true,
			changed_groups: ['metadata', 'organization'],
			preset_profile: presetProfile
		},
		isLoading: false,
		isError: false
	};
	h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
	h.activation = {
		data: null,
		isLoading: false,
		isError: false,
		isFetching: false,
		refetch: vi.fn()
	};
	const harmless = {
		current_settings_revision: 'settings-1',
		proposed_settings_revision: 'settings-2',
		stale: false,
		classification: 'harmless',
		preview_required: false,
		affected_root_ids: [],
		reasons: []
	};
	h.validate.mockResolvedValue(harmless);
	h.impact.mockResolvedValue(harmless);
	h.update.mockResolvedValue({ ...settings, settings_revision: 'settings-2' });
	h.createActivation.mockResolvedValue({
		job_id: 'preview-1',
		preview_token: 'token-1',
		expires_at: 2_000_000_000,
		operation_revision: 1
	});
	h.confirmActivation.mockResolvedValue({ ...settings, settings_revision: 'settings-2' });
});

describe('SettingsLibraryManagement', () => {
	it('starts off everywhere and preserves subordinate profile values while a master toggle is off', async () => {
		render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await expect.element(page.getByText('Off everywhere')).toBeVisible();
		await expect.element(page.getByText('Scanning: Automatic identification')).toBeVisible();

		await page.getByRole('button', { name: 'Edit' }).click();
		const profileDialog = page.getByRole('dialog', { name: 'Picard-style Organizer' });
		await expect
			.element(profileDialog.getByRole('heading', { name: 'Picard-style Organizer' }))
			.toHaveFocus();
		await expect.element(profileDialog.getByText('Customized from preset')).toBeVisible();
		await expect
			.element(profileDialog.getByText('Changed sections: metadata, file organization'))
			.toBeVisible();
		await profileDialog.getByText('Language reference').click();
		await expect.element(profileDialog.getByText('Alpha/Management Track.flac')).toBeVisible();
		await expect
			.element(profileDialog.getByText(/Run a dry preview for real file results/))
			.toBeVisible();
		const metadataToggle = profileDialog.getByRole('checkbox', { name: /Manage metadata tags/ });
		await metadataToggle.click();
		await expect
			.element(profileDialog.getByRole('combobox', { name: 'Mode for title' }))
			.not.toBeInTheDocument();
		await metadataToggle.click();
		await expect
			.element(profileDialog.getByRole('combobox', { name: 'Mode for title' }))
			.toHaveValue('merge');

		await profileDialog.getByText('Lyrics and loudness').click();
		const lyricsToggle = profileDialog.getByRole('checkbox', {
			name: /Fetch lyrics from LRCLIB/
		});
		const plainLyrics = profileDialog.getByRole('checkbox', { name: /Write plain lyrics/ });
		await expect.element(plainLyrics).toBeDisabled();
		await lyricsToggle.click();
		await expect.element(plainLyrics).toBeEnabled();
		await expect.element(plainLyrics).toBeChecked();

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

		await profileDialog.getByText('Preservation and format safety').click();
		await expect
			.element(profileDialog.getByText('DroppedNeedle catalog updates immediately'))
			.toBeVisible();
		await expect
			.element(profileDialog.getByRole('checkbox', { name: /Refresh DroppedNeedle/ }))
			.not.toBeInTheDocument();
	});

	it('adds a copied profile to the saved draft and opens it for editing', async () => {
		const settings = baseSettings();
		const sourceProfile = {
			...settings.profiles[0],
			id: '1c56cd00-4f7d-42ee-97df-2710110a31d2',
			name: 'Car copy profile',
			preset_origin: null,
			preset_version: null,
			revision: 'profile-custom'
		};
		settings.profiles.push(sourceProfile);
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
		const copiedProfile = {
			...sourceProfile,
			id: '94bf55a3-b553-4cf5-b18c-671194f67783',
			name: 'Archive profile',
			preset_origin: null,
			preset_version: null,
			revision: 'profile-2'
		};
		h.copy.mockResolvedValue({ profile: copiedProfile, settings_revision: 'settings-2' });

		render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('combobox', { name: 'Profile to copy' }).selectOptions(sourceProfile.id);
		await page.getByRole('textbox', { name: 'New profile name' }).fill('Archive profile');
		await page.getByRole('button', { name: 'Create copy' }).click();

		expect(h.copy).toHaveBeenCalledWith({
			profileId: sourceProfile.id,
			request: { name: 'Archive profile', expected_settings_revision: 'settings-1' }
		});
		const profileDialog = page.getByRole('dialog', { name: 'Archive profile', exact: true });
		await expect
			.element(profileDialog.getByRole('heading', { name: 'Archive profile' }))
			.toHaveFocus();
		await expect.element(page.getByText('Custom').last()).toBeVisible();
	});

	it('keeps refetched copies unique and removes a successfully deleted profile', async () => {
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

		render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		const profileRegion = page.getByRole('region', {
			name: 'Saved Library Management profiles'
		});
		await expect.element(profileRegion).toHaveAttribute('tabindex', '0');
		expect(profileRegion.getByRole('button', { name: /^Delete Profile/ }).all()).toHaveLength(4);

		await page.getByRole('textbox', { name: 'New profile name' }).fill('Profile 4');
		await page.getByRole('button', { name: 'Create copy' }).click();
		expect(profileRegion.getByRole('button', { name: 'Delete Profile 4' }).all()).toHaveLength(1);

		const profileDialog = page.getByRole('dialog', { name: 'Profile 4', exact: true });
		await profileDialog.getByRole('button', { name: 'Cancel' }).click();
		await profileRegion.getByRole('button', { name: 'Delete Profile 4' }).click();
		await page.getByRole('button', { name: 'Delete profile', exact: true }).click();

		expect(h.deleteProfile).toHaveBeenCalledWith({
			profileId: copies[3].id,
			request: { expected_settings_revision: 'settings-2' }
		});
		await expect
			.element(profileRegion.getByRole('button', { name: 'Delete Profile 4' }))
			.not.toBeInTheDocument();
	});

	it('resets one preset section in the draft and confirms before discarding changes', async () => {
		render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
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

	it('requires a current dry run and exact phrase before first automatic activation', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: ['automatic trigger enabled']
		});
		h.activation = {
			data: {
				job_id: 'preview-1',
				state: 'ready',
				ready_for_confirmation: true,
				expired: false,
				stale: false,
				summary: {
					eligible_count: 8,
					warning_count: 1,
					blocked_count: 0,
					path_change_count: 7
				}
			},
			isLoading: false,
			refetch: vi.fn()
		};

		render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure Library Management/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();

		expect(h.update).not.toHaveBeenCalled();
		await expect
			.element(page.getByRole('heading', { name: 'Enable Library Management' }))
			.toHaveFocus();
		await page.getByRole('button', { name: 'Run dry run' }).click();
		await expect.element(page.getByText('Eligible').first()).toBeVisible();
		expect(h.remember).toHaveBeenCalledWith('preview-1', 'token-1');
		await expect
			.element(page.getByRole('link', { name: 'Review file-by-file dry run' }))
			.toHaveAttribute('href', '/library/management/previews/preview-1');
		await page.getByRole('button', { name: 'Use this dry run' }).click();

		const enableButton = page.getByRole('button', { name: 'Enable Library Management' });
		await expect.element(enableButton).toBeDisabled();
		await page
			.getByRole('textbox', { name: /Type Enable Library Management/ })
			.fill('Enable Library Management');
		await expect.element(enableButton).toBeEnabled();
		await enableButton.click();
		expect(h.confirmActivation).toHaveBeenCalledWith(
			expect.objectContaining({
				confirmation: true,
				proofs: [{ root_id: 'root-1', job_id: 'preview-1', preview_token: 'token-1' }]
			})
		);
	});

	it('does not accept an expired or stale activation preview', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: []
		});
		h.activation = {
			data: {
				job_id: 'preview-1',
				state: 'ready',
				ready_for_confirmation: true,
				expired: true,
				stale: true,
				summary: { eligible_count: 8, warning_count: 0, blocked_count: 0, path_change_count: 8 }
			},
			isLoading: false,
			refetch: vi.fn()
		};

		render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure Library Management/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();
		await page.getByRole('button', { name: 'Run dry run' }).click();
		await expect.element(page.getByText(/stale or expired/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Use this dry run' })).toBeDisabled();
		await page.getByRole('button', { name: 'Run a fresh dry run' }).click();
		await expect.element(page.getByRole('button', { name: 'Run dry run' })).toBeVisible();
		expect(h.confirmActivation).not.toHaveBeenCalled();
	});

	it('labels a failed activation dry run as terminal and offers a fresh run', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: []
		});
		h.activation = {
			data: {
				job_id: 'preview-1',
				state: 'failed',
				ready_for_confirmation: false,
				expired: false,
				stale: false,
				summary: { eligible_count: 0, warning_count: 0, blocked_count: 0, path_change_count: 0 }
			},
			isLoading: false,
			isError: false,
			isFetching: false,
			refetch: vi.fn()
		};

		render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure Library Management/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();
		await page.getByRole('button', { name: 'Run dry run' }).click();

		await expect.element(page.getByText(/failed during planning/)).toBeVisible();
		await expect.element(page.getByText(/Planning is still running/)).not.toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Run a fresh dry run' })).toBeVisible();
	});

	it('shows a retryable activation-query failure and does not promise page-level resume', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: []
		});
		const refetch = vi.fn();
		h.activation = {
			data: null,
			isLoading: false,
			isError: true,
			isFetching: false,
			refetch
		};

		render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure Library Management/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();
		await page.getByRole('button', { name: 'Run dry run' }).click();

		await expect.element(page.getByText('Could not load this dry run.')).toBeVisible();
		await expect.element(page.getByText(/leave this page and return/)).not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Retry status' }).click();
		expect(refetch).toHaveBeenCalledOnce();
	});

	it('prevents activation dismissal while a destructive confirmation is pending', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: []
		});
		h.confirmActivationPending = true;

		render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure Library Management/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();

		await expect.element(page.getByRole('button', { name: 'Cancel', exact: true })).toBeDisabled();
		await expect
			.element(page.getByRole('button', { name: 'Cancel Library Management activation' }))
			.toBeDisabled();
	});

	it('keeps irreversible baseline purge in advanced retention with impact and typed confirmation', async () => {
		h.purgeData = {
			baseline_count: 14,
			referenced_blob_count: 9,
			referenced_blob_bytes: 4096,
			blocked_journal_count: 0,
			active_restore_count: 0,
			catalog_revision: 7,
			impact_token: 'impact-token'
		};
		h.purgeImpact.mockResolvedValue(h.purgeData);
		h.purge.mockResolvedValue({
			purged_baseline_count: 14,
			detached_reference_count: 9,
			cleaned_blob_count: 9,
			existing: false
		});

		render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByText('Retention, recycle, and refresh').click();
		await page.getByRole('button', { name: 'Purge baselines...' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Purge first-management baselines?' }))
			.toHaveFocus();
		await expect.element(page.getByText(/permanently removes 14 baselines/)).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Purge baselines', exact: true }))
			.toBeDisabled();
		await page.getByRole('textbox', { name: /PURGE BASELINES/ }).fill('PURGE BASELINES');
		await page.getByRole('button', { name: 'Purge baselines', exact: true }).click();

		expect(h.purge).toHaveBeenCalledWith(
			expect.objectContaining({
				impact_token: 'impact-token',
				expected_catalog_revision: 7,
				typed_confirmation: 'PURGE BASELINES'
			})
		);
	});
});
