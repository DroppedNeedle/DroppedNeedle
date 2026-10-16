// Shared by the Automation tab's specs: the mocked query, mutation and
// session-storage modules, and the settings they start from. Each spec mocks
// the modules with these objects, so they all drive one set of fakes.
import { vi } from 'vitest';

import type { LibraryManagementSettingsResponse } from '$lib/queries/library-management/types';
import { authStore, type AuthUser } from '$lib/stores/authStore.svelte';

export const h = {
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
	placement: {
		data: { roots: [] as Array<{ root_id: string; mode: string }> },
		isPending: false,
		isError: false,
		error: null,
		refetch: vi.fn()
	},
	checkPlacement: vi.fn(),
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
};

export const previewTokensMock = {
	rememberLibraryManagementPreviewToken: (...args: unknown[]) => h.remember(...args),
	rememberLibraryManagementActivationSession: (...args: unknown[]) => h.rememberSession(...args),
	readLibraryManagementActivationSession: (...args: unknown[]) => h.readSession(...args),
	forgetLibraryManagementActivationSession: (...args: unknown[]) => h.forgetSession(...args)
};

export const queriesMock = {
	getLibraryManagementSettingsQuery: () => h.settings,
	getLibraryManagementActivationPreviewQuery: () => h.activation,
	getLibraryManagementActivationHealthQuery: () => h.health,
	getLibraryManagementPlacementQuery: () => h.placement,
	getLibraryManagementOperationsQuery: () => h.operations,
	getLibraryManagementPresetDiffQuery: () => h.presetDiff
};

export const mutationsMock = {
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
	purgeLibraryManagementBaselinesMutation: () => ({ mutateAsync: h.purge, isPending: false }),
	checkLibraryManagementPlacementMutation: () => ({ mutate: h.checkPlacement, isPending: false })
};

export const profileId = 'c2741223-da7c-5231-bcf5-7cead27b07d9';
export const namingScriptId = '69202666-cb88-52b0-bac2-0afc62b1e909';
export const multiDiscNamingScriptId = '5b2bd6e2-4179-53bf-94aa-cfec47de8ab0';

export function baseSettings(): LibraryManagementSettingsResponse {
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
						constrained_genres_primary_only: true,
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
				notification: { refresh_droppedneedle: true, refresh_external_servers: false },
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
		settings_revision: 'settings-1',
		managed_fields: [
			{
				name: 'title',
				label: 'Title',
				source: 'Track title',
				mergeable: false,
				clearable: true,
				modes: ['disabled', 'replace', 'fill_missing']
			},
			{
				name: 'label',
				label: 'Label',
				source: 'Release labels',
				mergeable: true,
				clearable: true,
				modes: ['disabled', 'replace', 'fill_missing', 'merge']
			},
			{
				name: 'acoustid_id',
				label: 'AcoustID',
				source: "AcoustID lookup of the file's audio",
				mergeable: false,
				clearable: false,
				modes: ['disabled', 'replace', 'fill_missing']
			},
			{
				name: 'genre',
				label: 'Genre',
				source: 'The Genres section',
				mergeable: true,
				clearable: false,
				modes: ['disabled', 'replace', 'fill_missing', 'merge']
			}
		]
	};
}

export const roots = [
	{
		id: 'root-1',
		path: '/music/archive',
		label: 'Archive',
		policy: 'automatic' as const,
		rules: []
	}
];

export function activeAssignmentSettings(): LibraryManagementSettingsResponse {
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
			activation_confirmed_at: 1,
			activation_confirmed_by: null
		}
	];
	return settings;
}

export function adminUser(id: string): AuthUser {
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

export function resetLibraryManagementMocks(): void {
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
	h.placement.data = { roots: [] };
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
}
