import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { LibraryManagementSettingsResponse } from '$lib/queries/library-management/types';
import { authStore, type AuthUser } from '$lib/stores/authStore.svelte';

const h = vi.hoisted(() => ({
	settings: { data: {}, isLoading: false, isError: false, refetch: vi.fn() } as Record<
		string,
		unknown
	>,
	presetDiff: { data: null, isLoading: false, isError: false } as Record<string, unknown>,
	activation: { data: null, isLoading: false, refetch: vi.fn() } as Record<string, unknown>,
	health: {
		data: { stale_root_ids: [], blocked_root_ids: [] },
		isLoading: false,
		isFetching: false
	} as Record<string, unknown>,
	validate: vi.fn(),
	impact: vi.fn(),
	update: vi.fn(),
	copy: vi.fn(),
	deleteProfile: vi.fn(),
	exportProfile: vi.fn(),
	previewProfileImport: vi.fn(),
	importProfile: vi.fn(),
	createActivation: vi.fn(),
	confirmActivation: vi.fn(),
	stopActivation: vi.fn(),
	createActivationPending: false,
	confirmActivationPending: false,
	stopActivationPending: false,
	purgeImpact: vi.fn(),
	purge: vi.fn(),
	purgeData: null as Record<string, unknown> | null,
	remember: vi.fn(),
	rememberSession: vi.fn(),
	forgetSession: vi.fn(),
	readSession: vi.fn(),
	operations: {
		data: { pages: [{ items: [] as Array<Record<string, unknown>> }] },
		isLoading: false,
		isError: false
	} as Record<string, unknown>
}));

vi.mock('$lib/queries/library-management/LibraryManagementPreviewTokens', () => ({
	rememberLibraryManagementPreviewToken: (...args: unknown[]) => h.remember(...args),
	rememberLibraryManagementActivationSession: (...args: unknown[]) => h.rememberSession(...args),
	readLibraryManagementActivationSession: (...args: unknown[]) => h.readSession(...args),
	forgetLibraryManagementActivationSession: (...args: unknown[]) => h.forgetSession(...args)
}));
vi.mock('$lib/queries/library-management/LibraryManagementQueries.svelte', () => ({
	getLibraryManagementSettingsQuery: () => h.settings,
	getLibraryManagementActivationPreviewQuery: () => h.activation,
	getLibraryManagementActivationHealthQuery: () => h.health,
	getLibraryManagementOperationsQuery: () => h.operations,
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
	exportLibraryManagementProfileMutation: () => ({
		mutateAsync: h.exportProfile,
		isPending: false
	}),
	previewLibraryManagementProfileImportMutation: () => ({
		mutateAsync: h.previewProfileImport,
		isPending: false
	}),
	importLibraryManagementProfileMutation: () => ({
		mutateAsync: h.importProfile,
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
	controlLibraryManagementOperationMutation: () => ({
		mutateAsync: h.stopActivation,
		get isPending() {
			return h.stopActivationPending;
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
const namingScriptId = '69202666-cb88-52b0-bac2-0afc62b1e909';
const multiDiscNamingScriptId = '5b2bd6e2-4179-53bf-94aa-cfec47de8ab0';

function baseSettings(): LibraryManagementSettingsResponse {
	return {
		schema_version: 1,
		preset_catalog_version: 1,
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
					fields: [{ field: 'title', mode: 'fill_missing', clear_when_canonical_missing: false }],
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
						wav_tag_policy: 'id3'
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
					multi_disc_naming_script_id: multiDiscNamingScriptId,
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
						preserve_existing: false,
						required: false
					},
					replaygain: { enabled: false, mode: 'preserve', album_aware: true, required: false }
				},
				notification: { refresh_external_servers: false },
				identity: { automatic_edition_acceptance_enabled: false }
			}
		],
		default_profile_id: profileId,
		root_assignments: [],
		naming_scripts: [
			{
				id: namingScriptId,
				name: 'Picard-style: single disc',
				source:
					'{albumartist}/{album}{conditional(is_empty(year), "", concat(" (", year, ")"))}{conditional(is_empty(album_disambiguation), "", concat(" (", album_disambiguation, ")"))}/{track:02d} - {title}.{ext}',
				revision: 'script-1',
				preset_origin: 'picard_style_organizer',
				preset_version: 1
			},
			{
				id: multiDiscNamingScriptId,
				name: 'Picard-style: multiple discs',
				source:
					'{albumartist}/{album}{conditional(is_empty(year), "", concat(" (", year, ")"))}{conditional(is_empty(album_disambiguation), "", concat(" (", album_disambiguation, ")"))}/{default(medium_format, "Disc")} {medium_number:02d}/{track:02d} - {title}.{ext}',
				revision: 'script-multi-1',
				preset_origin: 'picard_style_organizer',
				preset_version: 3
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

function activeAssignmentSettings(): LibraryManagementSettingsResponse {
	const settings = baseSettings();
	settings.root_assignments = [
		{
			root_id: 'root-1',
			profile_id: null,
			overrides: null,
			enabled: true,
			automatic_acquisitions: true,
			automatic_drop_imports: false,
			automatic_scan_discovered: false,
			automatic_custom_editions: false,
			activation_profile_revision: 'stale-profile',
			activation_naming_policy_revision: null,
			activation_policy_revision: 'policy-1',
			activation_settings_revision: 'settings-0',
			activation_preview_token: 'old-token',
			activation_preview_hash: 'old-hash',
			activation_confirmed_at: 1
		}
	];
	return settings;
}

function adminUser(id: string): AuthUser {
	return {
		id,
		display_name: id,
		role: 'admin',
		email: null,
		avatar_url: null,
		username: id,
		username_display: id,
		providers: []
	};
}

beforeEach(() => {
	authStore.setUser(adminUser('admin-1'));
	vi.clearAllMocks();
	h.createActivationPending = false;
	h.confirmActivationPending = false;
	h.stopActivationPending = false;
	h.purgeData = null;
	h.readSession.mockReturnValue(null);
	h.operations = {
		data: { pages: [{ items: [] }] },
		isLoading: false,
		isError: false
	};
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
			version_upgrade_groups: ['organization'],
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
	h.health = {
		data: { stale_root_ids: [], blocked_root_ids: [] },
		isLoading: false,
		isFetching: false
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
	h.stopActivation.mockResolvedValue({ state: 'stopped' });
});

describe('SettingsLibraryManagement', () => {
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

	it('edits both naming slots and explains the multi-disc defaults', async () => {
		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('button', { name: 'Edit' }).click();
		const profileDialog = page.getByRole('dialog', { name: 'Picard-style Organizer' });
		await profileDialog.getByText('File naming and organization').click();

		await expect
			.element(profileDialog.getByRole('combobox', { name: 'Single-disc naming script' }))
			.toHaveValue(namingScriptId);
		const multiDisc = profileDialog.getByRole('combobox', {
			name: 'Multi-disc naming script'
		});
		await expect.element(multiDisc).toHaveValue(multiDiscNamingScriptId);
		await multiDisc.selectOptions('');
		await expect.element(multiDisc).toHaveValue('');
		await expect
			.element(profileDialog.getByText('Anthony Green/Avalon (2008)/03 - Drugdealer.flac'))
			.toBeVisible();
		await expect
			.element(profileDialog.getByText('Artist/Album (2023)/Disc 01/01 - Track.flac'))
			.toBeVisible();

		await profileDialog.getByText('Language reference').click();
		const valuesHelp = profileDialog.getByText(/Common values: title, album, album_disambiguation/);
		await expect.element(valuesHelp).toHaveTextContent('medium_format');
		await expect.element(valuesHelp).toHaveTextContent('medium_number');
		await expect.element(profileDialog.getByText(/concat, is_empty/)).toBeVisible();
		await expect
			.element(profileDialog.getByText(baseSettings().naming_scripts[1].source, { exact: true }))
			.toBeVisible();
		await expect.element(valuesHelp).toHaveTextContent('initial');
		await expect.element(profileDialog.getByText(/\{initial\}/)).toBeVisible();
		const initialHelp = profileDialog.getByText(/effective album artist/i);
		await expect.element(initialHelp).toBeVisible();
		await expect.element(initialHelp).toHaveTextContent(/leading.*The/i);
		await expect.element(initialHelp).toHaveTextContent(/Unicode whitespace/i);
		await expect.element(initialHelp).toHaveTextContent(/case-insensitive/i);
		await expect.element(initialHelp).toHaveTextContent(/one uppercase letter/i);
		await expect
			.element(initialHelp)
			.toHaveTextContent(/empty.*(?:nonletter|non[- ]?alphabetic).*#/i);
		await expect
			.element(profileDialog.getByText(/Run a dry preview for real file results/))
			.toBeVisible();

		await profileDialog.getByRole('button', { name: 'Save profile' }).click();
		expect(h.update).toHaveBeenCalledWith(
			expect.objectContaining({
				settings: expect.objectContaining({
					profiles: [
						expect.objectContaining({
							organization: expect.objectContaining({
								multi_disc_naming_script_id: null
							})
						})
					]
				})
			})
		);
	});

	it('shows both inert presets while keeping Picard-style as the library default', async () => {
		const settings = baseSettings();
		const complete = structuredClone(settings.profiles[0]);
		complete.id = '4c012b0e-509b-5f23-a759-65552c84db85';
		complete.name = 'Complete Library Organizer';
		complete.preset_origin = 'complete_library_organizer';
		complete.preset_version = 1;
		complete.enrichment.lyrics.enabled = true;
		complete.enrichment.replaygain.enabled = true;
		complete.enrichment.replaygain.mode = 'replace';
		settings.profiles.push(complete);
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		await expect
			.element(page.getByText('Complete Library Organizer', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByRole('radio', { name: 'Make Picard-style Organizer the library default' }))
			.toBeChecked();
		await expect
			.element(
				page.getByRole('radio', { name: 'Make Complete Library Organizer the library default' })
			)
			.not.toBeChecked();
		await expect
			.element(page.getByRole('button', { name: 'Delete Picard-style Organizer' }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('button', { name: 'Delete Complete Library Organizer' }))
			.not.toBeInTheDocument();
		await expect.element(page.getByText('Off everywhere')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Choose Management profile for Archive' }))
			.toHaveTextContent(/Inherited from library default.*Picard-style Organizer/);
	});

	it('offers only implemented editor choices and contextual safety guidance', async () => {
		const settings = baseSettings();
		settings.profiles[0].metadata.fields = [
			{ field: 'title', mode: 'replace', clear_when_canonical_missing: true },
			{ field: 'artist', mode: 'merge', clear_when_canonical_missing: false }
		];
		settings.profiles[0].artwork.providers = [
			'cover_art_archive_release',
			'cover_art_archive_release_group',
			'local_files',
			'embedded'
		];
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('button', { name: 'Edit' }).click();
		const profileDialog = page.getByRole('dialog', { name: 'Picard-style Organizer' });

		await profileDialog.getByText('Metadata fields').click();
		const titleMode = profileDialog.getByRole('combobox', { name: 'Mode for title' });
		const artistMode = profileDialog.getByRole('combobox', { name: 'Mode for artist' });
		await expect.element(titleMode).not.toHaveTextContent('Merge');
		await expect.element(titleMode).not.toHaveTextContent('Preserve');
		await expect.element(artistMode).toHaveTextContent('Merge');
		await expect
			.element(
				profileDialog.getByRole('checkbox', {
					name: 'Clear title when canonical value is missing'
				})
			)
			.toBeVisible();
		await titleMode.selectOptions('fill_missing');
		await expect
			.element(
				profileDialog.getByRole('checkbox', {
					name: 'Clear title when canonical value is missing'
				})
			)
			.not.toBeInTheDocument();

		await profileDialog.getByText('Credits and genres').click();
		await expect.element(profileDialog.getByText('Artist variations')).not.toBeInTheDocument();
		await profileDialog.getByText('Advanced genre rules').click();
		await expect
			.element(profileDialog.getByText('Primary genre on constrained formats'))
			.toBeVisible();

		await profileDialog.getByText('Artwork', { exact: true }).click();
		await expect.element(profileDialog.getByText('TheAudioDB')).not.toBeInTheDocument();
		const priority = profileDialog.getByRole('list', { name: 'Artwork source priority' });
		await expect
			.element(priority)
			.toHaveTextContent(
				/Exact-release artwork.*Release-group fallback.*Local files.*Existing embedded art/
			);
		await profileDialog.getByRole('button', { name: 'Move Exact-release artwork later' }).click();
		await expect
			.element(priority)
			.toHaveTextContent(
				/Release-group fallback.*Exact-release artwork.*Local files.*Existing embedded art/
			);
		await profileDialog.getByText('Advanced artwork rules').click();
		await expect.element(profileDialog.getByText(/A size of 0 means unlimited/)).toBeVisible();
		await expect
			.element(
				profileDialog
					.getByRole('group', { name: 'Preserve existing image types' })
					.getByRole('checkbox', { name: 'Front cover' })
			)
			.toBeVisible();
		await expect
			.element(profileDialog.getByRole('combobox', { name: 'External artwork naming script' }))
			.toHaveTextContent('Default album filenames (cover.jpg, back.jpg, …)');

		await profileDialog.getByText('Preservation and format safety').click();
		await profileDialog.getByText('Format compatibility').click();
		const id3Version = profileDialog.getByRole('combobox', { name: 'ID3 version' });
		const id3Encoding = profileDialog.getByRole('combobox', { name: 'ID3 text encoding' });
		await id3Version.selectOptions('2.3');
		await expect.element(id3Encoding).toHaveValue('utf16');
		await expect
			.element(profileDialog.getByRole('textbox', { name: 'ID3v2.3 list delimiter' }))
			.toBeVisible();
		await expect.element(id3Encoding).not.toHaveTextContent('UTF-8');
		await id3Version.selectOptions('2.4');
		await expect
			.element(profileDialog.getByRole('textbox', { name: 'ID3v2.3 list delimiter' }))
			.not.toBeInTheDocument();
		await expect.element(id3Encoding).toHaveTextContent('UTF-8');
		await expect.element(profileDialog.getByText(/Remove deletes the complete MP3/)).toBeVisible();
		await expect.element(profileDialog.getByText(/Do not write preserves raw AAC/)).toBeVisible();
		await expect.element(profileDialog.getByText(/may convert the active WAV/)).toBeVisible();

		await profileDialog.getByText('File naming and organization').click();
		await profileDialog.getByText('Path compatibility').click();
		await expect
			.element(profileDialog.getByText(/NFC preserves character distinctions/))
			.toBeVisible();
		await expect
			.element(profileDialog.getByText(/Changes only the filename extension/))
			.toBeVisible();
	});

	it('round-trips the explicit per-root multi-disc script mode', async () => {
		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByText('Per-root profile overrides').click();
		await page.getByRole('checkbox', { name: /Override selected profile values/ }).click();

		const mode = page.getByRole('combobox', { name: 'Multi-disc naming' });
		await expect.element(mode).toHaveValue('inherit');
		await expect
			.element(page.getByRole('combobox', { name: 'Selected multi-disc script' }))
			.not.toBeInTheDocument();
		await mode.selectOptions('script');
		const selectedScript = page.getByRole('combobox', { name: 'Selected multi-disc script' });
		await expect.element(selectedScript).toBeVisible();
		await selectedScript.selectOptions(multiDiscNamingScriptId);
		await page.getByRole('button', { name: 'Validate and save' }).click();

		expect(h.update).toHaveBeenCalledWith(
			expect.objectContaining({
				settings: expect.objectContaining({
					root_assignments: [
						expect.objectContaining({
							overrides: expect.objectContaining({
								multi_disc_naming_mode: 'script',
								multi_disc_naming_script_id: multiDiscNamingScriptId
							})
						})
					]
				})
			})
		);
	});

	it('serializes the per-root effective-standard mode without a script ID', async () => {
		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByText('Per-root profile overrides').click();
		await page.getByRole('checkbox', { name: /Override selected profile values/ }).click();
		await page.getByRole('combobox', { name: 'Multi-disc naming' }).selectOptions('standard');
		await page.getByRole('button', { name: 'Validate and save' }).click();

		expect(h.update).toHaveBeenCalledWith(
			expect.objectContaining({
				settings: expect.objectContaining({
					root_assignments: [
						expect.objectContaining({
							overrides: expect.objectContaining({
								multi_disc_naming_mode: 'standard',
								multi_disc_naming_script_id: null
							})
						})
					]
				})
			})
		);
	});

	it('sets the library default from the profile cards and updates inherited roots', async () => {
		const settings = baseSettings();
		const lyricsProfile = structuredClone(settings.profiles[0]);
		lyricsProfile.id = '1c56cd00-4f7d-42ee-97df-2710110a31d2';
		lyricsProfile.name = 'Picard-style Organizer + Lyrics';
		lyricsProfile.description = 'Canonical organization with synchronized lyrics.';
		lyricsProfile.preset_origin = null;
		lyricsProfile.preset_version = null;
		lyricsProfile.enrichment.lyrics.enabled = true;
		settings.profiles.push(lyricsProfile);
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		const lyricsCard = page.getByRole('article', { name: 'Picard-style Organizer + Lyrics' });
		await expect.element(lyricsCard).toHaveAttribute('data-default', 'false');
		await lyricsCard
			.getByRole('radio', {
				name: 'Make Picard-style Organizer + Lyrics the library default'
			})
			.click();

		await expect.element(lyricsCard).toHaveAttribute('data-default', 'true');
		await expect.element(lyricsCard.getByText('Default after save')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Validate and save' })).toBeVisible();

		const rootPicker = page.getByRole('button', {
			name: 'Choose Management profile for Archive'
		});
		await expect.element(rootPicker).toHaveTextContent('Inherited from library default');
		await expect.element(rootPicker).toHaveTextContent('Picard-style Organizer + Lyrics');
		await rootPicker.click();
		const rootChoices = page.getByRole('group', {
			name: 'Management profile for Archive choices'
		});
		await expect
			.element(
				rootChoices.getByRole('radio', {
					name: 'Use library default: Picard-style Organizer + Lyrics'
				})
			)
			.toBeChecked();
		await rootChoices
			.getByRole('radio', { name: 'Use Picard-style Organizer', exact: true })
			.click();

		await expect.element(rootPicker).toHaveTextContent('Explicit root profile');
		await expect.element(rootPicker).toHaveTextContent('Unsaved choice');
	});

	it('restores an activation draft after the settings page remounts', async () => {
		const response = baseSettings();
		const { settings_revision: _settingsRevision, ...savedSettings } = response;
		const activationSettings = structuredClone(savedSettings);
		activationSettings.root_assignments = [
			{
				root_id: 'root-1',
				profile_id: profileId,
				overrides: null,
				enabled: true,
				automatic_acquisitions: true,
				automatic_drop_imports: false,
				automatic_scan_discovered: false,
				automatic_custom_editions: false,
				activation_profile_revision: null,
				activation_naming_policy_revision: null,
				activation_policy_revision: null,
				activation_settings_revision: null,
				activation_preview_token: null,
				activation_preview_hash: null,
				activation_confirmed_at: null
			}
		];
		h.readSession.mockReturnValue({
			sourceRevision: 'settings-1',
			policyRevision: 'policy-1',
			draft: activationSettings,
			activationDraft: activationSettings,
			rootIds: ['root-1'],
			rootIndex: 0,
			jobId: 'preview-1',
			previewToken: 'token-1',
			proofs: []
		});
		h.activation = {
			data: {
				job_id: 'preview-1',
				state: 'running',
				worker_stalled: false,
				control_request: 'none',
				ready_for_confirmation: false,
				expired: false,
				stale: false,
				summary: { item_count: 1000, bundle_count: 111 }
			},
			isLoading: false,
			isError: false,
			isFetching: false,
			refetch: vi.fn()
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		await expect.element(page.getByText('Dry run planning', { exact: true })).toBeVisible();
		await expect.element(page.getByText(/files found/)).toHaveTextContent(/1,000.*files found/);
		await expect.element(page.getByRole('button', { name: 'View dry run' })).toBeVisible();
		await expect
			.element(page.getByRole('checkbox', { name: /Configure file organization/ }))
			.toBeChecked();
	});

	it('keeps activation sessions isolated when the authenticated admin changes', async () => {
		const response = baseSettings();
		const { settings_revision: _settingsRevision, ...savedSettings } = response;
		const activationSettings = structuredClone(savedSettings);
		activationSettings.root_assignments = [
			{
				root_id: 'root-1',
				profile_id: profileId,
				overrides: null,
				enabled: true,
				automatic_acquisitions: true,
				automatic_drop_imports: false,
				automatic_scan_discovered: false,
				automatic_custom_editions: false,
				activation_profile_revision: null,
				activation_naming_policy_revision: null,
				activation_policy_revision: null,
				activation_settings_revision: null,
				activation_preview_token: null,
				activation_preview_hash: null,
				activation_confirmed_at: null
			}
		];
		h.readSession.mockImplementation((userId: string) =>
			userId === 'admin-1'
				? {
						sourceRevision: 'settings-1',
						policyRevision: 'policy-1',
						draft: activationSettings,
						activationDraft: activationSettings,
						rootIds: ['root-1'],
						rootIndex: 0,
						jobId: 'preview-1',
						previewToken: 'token-1',
						proofs: []
					}
				: null
		);

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await vi.waitFor(() => expect(h.readSession).toHaveBeenCalledWith('admin-1'));
		h.rememberSession.mockClear();

		authStore.setUser(adminUser('admin-2'));

		await vi.waitFor(() => expect(h.readSession).toHaveBeenCalledWith('admin-2'));
		await vi.waitFor(() => expect(h.forgetSession).toHaveBeenCalledWith('admin-2'));
		expect(h.rememberSession).not.toHaveBeenCalledWith(
			'admin-2',
			expect.objectContaining({ previewToken: 'token-1' })
		);
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

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
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
		await expect.element(profileDialog.getByText('Custom', { exact: true })).toBeVisible();
	});

	it('exports a saved profile as a file-first bundle with a copyable code', async () => {
		h.exportProfile.mockResolvedValue({
			filename: 'picard-style-organizer.dnprofile',
			mime_type: 'application/vnd.droppedneedle.profile+json',
			document: '{"format":"droppedneedle-library-profile"}',
			share_code: 'DNLP1:portable-code',
			bundle_hash: 'a'.repeat(64),
			settings_revision: 'settings-1'
		});
		const createObjectUrl = vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:profile');
		const revokeObjectUrl = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => undefined);
		const writeText = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue(undefined);

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('button', { name: 'Share Picard-style Organizer' }).click();

		const dialog = page.getByRole('dialog', { name: 'Share Picard-style Organizer' });
		await expect
			.element(dialog.getByRole('heading', { name: 'Share Picard-style Organizer' }))
			.toHaveFocus();
		await expect.element(dialog.getByText('Profile file', { exact: true })).toBeVisible();
		await expect.element(dialog.getByText(/wherever you share the profile/)).toBeVisible();
		await expect.element(dialog.getByText(/Discord/)).not.toBeInTheDocument();
		expect(h.exportProfile).toHaveBeenCalledWith({
			profileId,
			request: { expected_settings_revision: 'settings-1' }
		});
		await expect.element(dialog.getByRole('button', { name: 'Download .dnprofile' })).toBeVisible();
		await dialog.getByRole('button', { name: 'Download .dnprofile' }).click();
		expect(createObjectUrl).toHaveBeenCalledWith(expect.any(Blob));
		expect(revokeObjectUrl).toHaveBeenCalledWith('blob:profile');
		await dialog.getByText('Show text code').click();
		await expect
			.element(dialog.getByRole('textbox', { name: 'Profile share code' }))
			.toHaveValue('DNLP1:portable-code');
		await dialog.getByRole('button', { name: 'Copy code' }).click();
		expect(writeText).toHaveBeenCalledWith('DNLP1:portable-code');
		await expect.element(dialog.getByRole('button', { name: 'Code copied' })).toBeVisible();
		await expect.element(dialog.getByTestId('profile-code-copied-icon')).toBeVisible();
		await dialog.getByRole('button', { name: 'Close', exact: true }).click();
		await page.getByRole('button', { name: 'Share Picard-style Organizer' }).click();
		await expect
			.element(
				page
					.getByRole('dialog', { name: 'Share Picard-style Organizer' })
					.getByRole('button', { name: 'Copy code' })
			)
			.toBeVisible();
		writeText.mockRejectedValueOnce(new Error('Clipboard unavailable'));
		await page
			.getByRole('dialog', { name: 'Share Picard-style Organizer' })
			.getByRole('button', { name: 'Copy code' })
			.click();
		await expect
			.element(
				page
					.getByRole('dialog', { name: 'Share Picard-style Organizer' })
					.getByRole('button', { name: 'Copy code' })
			)
			.toBeVisible();
		createObjectUrl.mockRestore();
		revokeObjectUrl.mockRestore();
		writeText.mockRestore();
	});

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

	it('blocks profile import while Library Management has unsaved changes', async () => {
		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();

		await expect.element(page.getByRole('button', { name: 'Import profile' })).toBeDisabled();
		await expect
			.element(
				page.getByText('Save or discard current organization changes before importing a profile.')
			)
			.toBeVisible();
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
						activation_confirmed_at: null
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
		const activationData = {
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
		};
		h.activation = {
			data: activationData,
			isLoading: false,
			refetch: vi.fn(async () => ({ data: activationData, error: null }))
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();

		expect(h.update).not.toHaveBeenCalled();
		await expect
			.element(page.getByRole('heading', { name: 'Enable file organization' }))
			.toHaveFocus();
		await page.getByRole('button', { name: 'Run dry run' }).click();
		await expect.element(page.getByText('Eligible').first()).toBeVisible();
		expect(h.remember).toHaveBeenCalledWith('preview-1', 'token-1');
		await expect
			.element(page.getByRole('link', { name: 'Review file-by-file dry run' }))
			.toHaveAttribute('href', '/library/management/previews/preview-1');
		await page.getByRole('button', { name: 'Use this dry run' }).click();

		const enableButton = page.getByRole('button', { name: 'Enable file organization' });
		await expect.element(enableButton).toBeDisabled();
		await page.getByRole('textbox', { name: /Type CONFIRM/ }).fill('Enable file organization');
		await expect.element(enableButton).toBeDisabled();
		await page.getByRole('textbox', { name: /Type CONFIRM/ }).fill('CONFIRM');
		await expect.element(enableButton).toBeEnabled();
		await enableButton.click();
		expect(h.confirmActivation).toHaveBeenCalledWith(
			expect.objectContaining({
				confirmation: true,
				proofs: [{ root_id: 'root-1', job_id: 'preview-1', preview_token: 'token-1' }]
			})
		);
	});

	it('offers a fresh dry run when a saved activation goes stale', async () => {
		const settings = activeAssignmentSettings();
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
		h.health = {
			data: { stale_root_ids: ['root-1'], blocked_root_ids: [] },
			isLoading: false,
			isFetching: false
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		await expect.element(page.getByText('Activation stale — run a dry run')).toBeVisible();
		await expect.element(page.getByText('Automatic writes paused — dry run needed')).toBeVisible();
		await expect
			.element(page.getByText(/Archive no longer matches its confirmed dry run/))
			.toBeVisible();
		await page.getByRole('button', { name: 'Run a fresh dry run' }).click();

		await expect
			.element(page.getByRole('heading', { name: 'Enable file organization' }))
			.toHaveFocus();
		await page.getByRole('button', { name: 'Run dry run' }).click();
		expect(h.createActivation).toHaveBeenCalledWith(expect.objectContaining({ root_id: 'root-1' }));
	});

	it('disables the fresh dry run while activation health is rechecking', async () => {
		const settings = activeAssignmentSettings();
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
		h.health = {
			data: { stale_root_ids: ['root-1'], blocked_root_ids: [] },
			isLoading: false,
			isFetching: true
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		await expect.element(page.getByRole('button', { name: 'Run a fresh dry run' })).toBeDisabled();
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

		await expect.element(page.getByText('Paused — needs attention', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Automatic writes paused — needs attention')).toBeVisible();
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

	it('shows a neutral checking state while activation health loads', async () => {
		const settings = activeAssignmentSettings();
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
		h.health = { data: undefined, isLoading: true, isFetching: true, isError: false };

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		await expect.element(page.getByText('Checking activation status…')).toBeVisible();
		await expect.element(page.getByText('Checking activation…')).toBeVisible();
		await expect.element(page.getByText('Configuration saved')).not.toBeInTheDocument();
	});

	it('offers a retry when the activation status check fails', async () => {
		const refetch = vi.fn();
		h.health = {
			data: undefined,
			isLoading: false,
			isFetching: false,
			isError: true,
			refetch
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		await expect.element(page.getByText('Could not check activation status')).toBeVisible();
		await expect.element(page.getByText('Configuration saved')).not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Retry status check' }).click();
		expect(refetch).toHaveBeenCalled();
	});

	it('saves another trigger immediately when the write profile is already authorized', async () => {
		const settings = baseSettings();
		settings.root_assignments = [
			{
				root_id: 'root-1',
				profile_id: profileId,
				overrides: null,
				enabled: true,
				automatic_acquisitions: true,
				automatic_drop_imports: false,
				automatic_scan_discovered: false,
				automatic_custom_editions: false,
				activation_profile_revision: 'profile-1',
				activation_naming_policy_revision: 'script-1',
				activation_policy_revision: 'policy-1',
				activation_settings_revision: 'settings-1',
				activation_preview_token: 'verified',
				activation_preview_hash: 'preview-hash',
				activation_confirmed_at: 1
			}
		];
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'harmless',
			preview_required: false,
			affected_root_ids: ['root-1'],
			reasons: ['The authorized write profile is unchanged.']
		});
		const saved = structuredClone(settings);
		saved.settings_revision = 'settings-2';
		saved.root_assignments[0].automatic_drop_imports = true;
		h.update.mockResolvedValue(saved);

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		await page.getByRole('checkbox', { name: /Drop & Free imports/ }).click();
		await expect.element(page.getByText(/Trigger-only changes save immediately/)).toBeVisible();
		await page.getByRole('button', { name: 'Validate and save' }).click();

		expect(h.createActivation).not.toHaveBeenCalled();
		expect(h.update).toHaveBeenCalledWith(
			expect.objectContaining({
				expected_settings_revision: 'settings-1',
				settings: expect.objectContaining({
					root_assignments: [
						expect.objectContaining({
							root_id: 'root-1',
							automatic_drop_imports: true
						})
					]
				})
			})
		);
		await expect
			.element(page.getByRole('heading', { name: 'Enable file organization' }))
			.not.toBeInTheDocument();
		await expect.element(page.getByText('Configuration saved')).toBeVisible();
	});

	it('clears Custom edition automation when Scan-discovered is disabled', async () => {
		const settings = baseSettings();
		settings.root_assignments = [
			{
				root_id: 'root-1',
				profile_id: profileId,
				overrides: null,
				enabled: true,
				automatic_acquisitions: true,
				automatic_drop_imports: false,
				automatic_scan_discovered: true,
				automatic_custom_editions: true,
				activation_profile_revision: 'profile-1',
				activation_naming_policy_revision: 'script-1',
				activation_policy_revision: 'policy-1',
				activation_settings_revision: 'settings-1',
				activation_preview_token: 'verified',
				activation_preview_hash: 'preview-hash',
				activation_confirmed_at: 1
			}
		];
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		const custom = page.getByRole('checkbox', { name: /Include Custom editions/ });
		await expect.element(custom).toBeChecked();
		await page.getByRole('checkbox', { name: /Scan-discovered/ }).click();
		await expect.element(custom).not.toBeChecked();
		await expect.element(custom).toBeDisabled();
	});

	it('rechecks a ready dry run before accepting it for activation', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: ['automatic trigger enabled']
		});
		const ready = {
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
		};
		const refetch = vi.fn(async () => ({
			data: { ...ready, ready_for_confirmation: false, stale: true },
			error: null
		}));
		h.activation = { data: ready, isLoading: false, refetch };

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();
		await page.getByRole('button', { name: 'Run dry run' }).click();
		await page.getByRole('button', { name: 'Use this dry run' }).click();

		expect(refetch).toHaveBeenCalledOnce();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('This dry run is no longer current.');
		await expect
			.element(page.getByRole('textbox', { name: /Type CONFIRM/ }))
			.not.toBeInTheDocument();
	});

	it('shows automatic planning progress and resumes the same dry run after closing', async () => {
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
				state: 'running',
				worker_stalled: false,
				control_request: 'none',
				operation_row_revision: 2,
				ready_for_confirmation: false,
				expired: false,
				stale: false,
				summary: {
					item_count: 1500,
					bundle_count: 169,
					eligible_count: 0,
					warning_count: 0,
					blocked_count: 0,
					path_change_count: 0
				}
			},
			isLoading: false,
			isError: false,
			isFetching: false,
			refetch: vi.fn()
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();
		await page.getByRole('button', { name: 'Run dry run' }).click();

		await expect.element(page.getByRole('status')).toHaveTextContent(/updates automatically/i);
		await expect.element(page.getByText('1,500 files', { exact: true })).toBeVisible();
		await expect.element(page.getByText('169 release bundles', { exact: true })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Use this dry run' })).toBeDisabled();
		await expect
			.element(page.getByRole('button', { name: 'Refresh status' }))
			.not.toBeInTheDocument();

		await page.getByRole('button', { name: 'Close', exact: true }).click();
		await expect.element(page.getByText('Dry run planning', { exact: true })).toBeVisible();
		await page.getByRole('button', { name: 'View dry run' }).click();

		await expect.element(page.getByRole('status')).toHaveTextContent(/updates automatically/i);
		await expect.element(page.getByRole('button', { name: 'Run dry run' })).not.toBeInTheDocument();
		expect(h.createActivation).toHaveBeenCalledOnce();
	});

	it('cannot authorize settings edited after a dry run started', async () => {
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
				state: 'running',
				worker_stalled: false,
				control_request: 'none',
				operation_row_revision: 2,
				ready_for_confirmation: false,
				expired: false,
				stale: false,
				summary: { item_count: 20, bundle_count: 2 }
			},
			isLoading: false,
			isError: false,
			isFetching: false,
			refetch: vi.fn()
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();
		await page.getByRole('button', { name: 'Run dry run' }).click();
		await page.getByRole('button', { name: 'Close', exact: true }).click();

		await page.getByRole('checkbox', { name: /Drop & Free imports/ }).click();
		await expect
			.element(page.getByText('Configuration changed after this dry run started'))
			.toBeVisible();
		await page.getByRole('button', { name: 'Restart review' }).click();

		expect(h.stopActivation).toHaveBeenCalledWith({ jobId: 'preview-1', expectedRevision: 2 });
		await expect.element(page.getByRole('button', { name: 'Run dry run' })).toBeVisible();
		expect(h.createActivation).toHaveBeenCalledOnce();
	});

	it('shows durable progress and a safe recovery action when the worker lease expires', async () => {
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
				state: 'running',
				control_request: 'none',
				operation_row_revision: 5,
				ready_for_confirmation: false,
				expired: false,
				stale: false,
				worker_stalled: true,
				summary: {
					item_count: 1500,
					bundle_count: 169,
					eligible_count: 0,
					warning_count: 0,
					blocked_count: 0,
					path_change_count: 0
				}
			},
			isLoading: false,
			isError: false,
			isFetching: false,
			refetch: vi.fn()
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();
		await page.getByRole('button', { name: 'Run dry run' }).click();

		const dialog = page.getByRole('dialog', { name: 'Enable file organization' });
		await expect.element(dialog.getByRole('alert')).toHaveTextContent(/stopped responding/i);
		await expect.element(dialog.getByRole('alert')).toHaveTextContent(/1,500 files/i);
		await expect.element(dialog.getByText('Eligible', { exact: true })).not.toBeInTheDocument();
		await expect.element(dialog.getByRole('button', { name: 'Stop dry run' })).toBeEnabled();

		await dialog.getByRole('button', { name: 'Close', exact: true }).click();
		await expect.element(page.getByText('Dry run interrupted', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText(/files found/))
			.toHaveTextContent(/1,500.*files found.*169.*release bundles/);
	});

	it('distinguishes queued work and can stop the durable dry run', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: ['automatic trigger enabled']
		});
		const refetch = vi.fn();
		h.activation = {
			data: {
				job_id: 'preview-1',
				state: 'queued',
				control_request: 'none',
				operation_row_revision: 7,
				ready_for_confirmation: false,
				expired: false,
				stale: false,
				summary: {
					eligible_count: 0,
					warning_count: 0,
					blocked_count: 0,
					path_change_count: 0
				}
			},
			isLoading: false,
			isError: false,
			isFetching: false,
			refetch
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();
		await page.getByRole('button', { name: 'Run dry run' }).click();

		await expect.element(page.getByRole('status')).toHaveTextContent(/Queued for planning/);
		await page.getByRole('button', { name: 'Stop dry run' }).click();
		expect(h.stopActivation).toHaveBeenCalledWith({
			jobId: 'preview-1',
			expectedRevision: 7
		});
		expect(refetch).toHaveBeenCalledOnce();
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

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
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

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
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

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
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

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();

		await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toBeDisabled();
		await expect
			.element(page.getByRole('button', { name: 'Close organization activation' }))
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

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByText('Retention, recycle, and refresh').click();
		await page.getByRole('button', { name: 'Purge baselines...' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Purge original baselines?' }))
			.toHaveFocus();
		await expect.element(page.getByText(/permanently removes 14 baselines/)).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Purge baselines', exact: true }))
			.toBeDisabled();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('PURGE BASELINES');
		await expect
			.element(page.getByRole('button', { name: 'Purge baselines', exact: true }))
			.toBeDisabled();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('CONFIRM');
		await page.getByRole('button', { name: 'Purge baselines', exact: true }).click();

		expect(h.purge).toHaveBeenCalledWith(
			expect.objectContaining({
				impact_token: 'impact-token',
				expected_catalog_revision: 7,
				typed_confirmation: 'CONFIRM'
			})
		);
	});
});
