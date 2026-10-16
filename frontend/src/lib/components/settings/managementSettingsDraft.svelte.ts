/**
 * The Automation tab's working copy of the library management settings: the
 * draft being edited, the last saved copy and the revision both came from.
 * A newer revision from the server replaces both. Saving runs in the tab,
 * which checks the change first and may send it through the activation
 * wizard instead.
 */
import { untrack } from 'svelte';

import type { LibraryRootSettings } from '$lib/queries/library/LibraryOperationsTypes';
import type {
	LibraryManagementProfile,
	LibraryManagementProfileImportResponse,
	LibraryManagementRootAssignment,
	LibraryManagementRootOverrides,
	LibraryManagementSettings,
	LibraryManagementSettingsResponse,
	ManagementScriptSettings
} from '$lib/queries/library-management/types';

export function settingsPayload(
	response: LibraryManagementSettingsResponse
): LibraryManagementSettings {
	const value = structuredClone(response);
	const { settings_revision: _revision, managed_fields: _fields, ...settings } = value;
	return settings;
}

/** True when a root is set up and at least one automatic trigger is on. */
export function writesAutomatically(assignment: LibraryManagementRootAssignment): boolean {
	return (
		assignment.enabled &&
		(assignment.automatic_acquisitions ||
			assignment.automatic_drop_imports ||
			assignment.automatic_scan_discovered)
	);
}

export function profileAspects(profile: LibraryManagementProfile): string[] {
	const aspects: string[] = [];
	if (profile.metadata.enabled) aspects.push('tags');
	if (profile.genres.enabled) aspects.push('genres');
	if (profile.artwork.embedded_enabled || profile.artwork.external_enabled) aspects.push('artwork');
	if (profile.enrichment.lyrics.enabled) aspects.push('lyrics');
	if (profile.enrichment.replaygain.enabled) aspects.push('ReplayGain');
	if (profile.organization.rename_enabled) aspects.push('rename');
	if (profile.organization.move_enabled) aspects.push('move');
	return aspects;
}

export function emptyOverrides(): LibraryManagementRootOverrides {
	return {
		metadata_enabled: null,
		genres_enabled: null,
		embedded_artwork_enabled: null,
		external_artwork_enabled: null,
		rename_enabled: null,
		move_enabled: null,
		move_sidecars: null,
		source_cleanup: null,
		preserve_timestamps: null,
		naming_script_id: null,
		multi_disc_naming_mode: 'inherit',
		multi_disc_naming_script_id: null,
		automatic_edition_acceptance_enabled: null
	};
}

function emptyAssignment(rootId: string): LibraryManagementRootAssignment {
	return {
		root_id: rootId,
		profile_id: null,
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
	};
}

function upsertProfile(
	current: LibraryManagementProfile[],
	profile: LibraryManagementProfile
): LibraryManagementProfile[] {
	const saved = structuredClone(profile);
	return current.some((value) => value.id === saved.id)
		? current.map((value) => (value.id === saved.id ? saved : value))
		: [...current, saved];
}

function upsertScripts(
	current: ManagementScriptSettings[],
	created: ManagementScriptSettings[]
): ManagementScriptSettings[] {
	const createdIds = new Set(created.map((script) => script.id));
	return [...current.filter((script) => !createdIds.has(script.id)), ...structuredClone(created)];
}

export class ManagementSettingsDraft {
	draft = $state<LibraryManagementSettings | null>(null);
	persisted = $state<LibraryManagementSettings | null>(null);
	sourceRevision = $state('');
	/** The profile the copy row starts from. */
	copySourceProfileId = $state('');
	saveError = $state('');

	readonly profiles = $derived(this.draft?.profiles ?? []);
	readonly defaultProfile = $derived(
		this.profiles.find((profile) => profile.id === this.draft?.default_profile_id) ?? null
	);
	readonly activeAssignments = $derived(
		(this.persisted?.root_assignments ?? []).filter(writesAutomatically)
	);
	readonly hasUnsaved = $derived(
		Boolean(
			this.draft &&
			this.persisted &&
			JSON.stringify($state.snapshot(this.draft)) !==
				JSON.stringify($state.snapshot(this.persisted))
		)
	);

	/** Takes the server's settings as both the draft and the saved copy. */
	load(response: LibraryManagementSettingsResponse): void {
		this.accept(response);
		const copySource = untrack(() => this.copySourceProfileId);
		if (!response.profiles.some((profile) => profile.id === copySource)) {
			this.copySourceProfileId = response.default_profile_id;
		}
	}

	/** Loads the server's settings when their revision moved on. */
	sync(response: LibraryManagementSettingsResponse): void {
		if (response.settings_revision !== untrack(() => this.sourceRevision)) this.load(response);
	}

	/** Takes a save's answer as both the draft and the saved copy. */
	accept(saved: LibraryManagementSettingsResponse): void {
		this.draft = settingsPayload(saved);
		this.persisted = settingsPayload(saved);
		this.sourceRevision = saved.settings_revision;
	}

	acceptDelete(saved: LibraryManagementSettingsResponse, deletedProfileId: string): void {
		this.accept(saved);
		if (this.copySourceProfileId === deletedProfileId) {
			this.copySourceProfileId = saved.default_profile_id;
		}
	}

	/** Adds a copied profile to the draft and the saved copy, keeping other edits. */
	acceptCopy(result: { profile: LibraryManagementProfile; settings_revision: string }): void {
		if (!this.draft) return;
		this.draft = { ...this.draft, profiles: upsertProfile(this.draft.profiles, result.profile) };
		if (this.persisted) {
			this.persisted = {
				...this.persisted,
				profiles: upsertProfile(this.persisted.profiles, result.profile)
			};
		}
		this.sourceRevision = result.settings_revision;
	}

	acceptImport(result: LibraryManagementProfileImportResponse): void {
		if (!this.draft) return;
		this.draft = {
			...this.draft,
			profiles: upsertProfile(this.draft.profiles, result.profile),
			naming_scripts: upsertScripts(this.draft.naming_scripts, result.naming_scripts),
			tagging_scripts: upsertScripts(this.draft.tagging_scripts, result.tagging_scripts)
		};
		if (this.persisted) {
			this.persisted = {
				...this.persisted,
				profiles: upsertProfile(this.persisted.profiles, result.profile),
				naming_scripts: upsertScripts(this.persisted.naming_scripts, result.naming_scripts),
				tagging_scripts: upsertScripts(this.persisted.tagging_scripts, result.tagging_scripts)
			};
		}
		this.sourceRevision = result.settings_revision;
		this.copySourceProfileId = result.profile.id;
	}

	assignmentFor(rootId: string): LibraryManagementRootAssignment {
		return (
			this.draft?.root_assignments.find((assignment) => assignment.root_id === rootId) ??
			emptyAssignment(rootId)
		);
	}

	persistedAssignmentFor(rootId: string): LibraryManagementRootAssignment {
		return (
			this.persisted?.root_assignments.find((assignment) => assignment.root_id === rootId) ??
			emptyAssignment(rootId)
		);
	}

	updateAssignment(
		rootId: string,
		update: (assignment: LibraryManagementRootAssignment) => LibraryManagementRootAssignment
	): void {
		if (!this.draft) return;
		const next = update($state.snapshot(this.assignmentFor(rootId)));
		const exists = this.draft.root_assignments.some((assignment) => assignment.root_id === rootId);
		this.draft = {
			...this.draft,
			root_assignments: exists
				? this.draft.root_assignments.map((assignment) =>
						assignment.root_id === rootId ? next : assignment
					)
				: [...this.draft.root_assignments, next]
		};
	}

	updateOverrides(rootId: string, update: Partial<LibraryManagementRootOverrides>): void {
		this.updateAssignment(rootId, (assignment) => ({
			...assignment,
			overrides: { ...(assignment.overrides ?? emptyOverrides()), ...update }
		}));
	}

	setDefaultProfile(profileId: string): void {
		if (!this.draft) return;
		this.draft = { ...this.draft, default_profile_id: profileId };
	}

	/** The roots that use a profile, by name or through the library default. */
	rootLabelsFor(profileId: string, roots: LibraryRootSettings[]): string[] {
		const draft = this.draft;
		if (!draft) return [];
		return roots.flatMap((root) => {
			const assignment = draft.root_assignments.find((value) => value.root_id === root.id);
			return assignment && (assignment.profile_id ?? draft.default_profile_id) === profileId
				? [root.label]
				: [];
		});
	}

	/** Why a custom profile cannot be deleted, or null when it can. */
	deleteBlocker(profile: LibraryManagementProfile, roots: LibraryRootSettings[]): string | null {
		if (!this.draft || profile.preset_origin) return null;
		if (this.profiles.length === 1) return 'Only remaining profile';
		if (profile.id === this.draft.default_profile_id) return 'Library default';
		if (this.rootLabelsFor(profile.id, roots).length > 0) return 'Assigned to root';
		return null;
	}
}
