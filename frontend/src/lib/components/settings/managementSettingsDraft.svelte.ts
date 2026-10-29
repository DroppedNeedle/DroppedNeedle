/**
 * The Automation tab's settings, on the shared settings-section helper: one
 * draft that the profile list, the root cards and the profile editor all
 * edit, plus the revision a save is checked against.
 *
 * The revision is the one the draft was loaded from. A refetch that brings a
 * newer revision while the draft has unsaved edits keeps the edits and the
 * old revision, so saving them is refused as stale rather than overwriting
 * the newer settings. Discard loads the newer ones.
 *
 * Call while a component initialises: it creates the query, the mutation and
 * the helper's effects.
 */
import { untrack } from 'svelte';

import type { LibraryRootSettings } from '$lib/queries/library/LibraryOperationsTypes';
import { getLibraryManagementSettingsQuery } from '$lib/queries/library-management/LibraryManagementQueries.svelte';
import { updateLibraryManagementSettingsMutation } from '$lib/queries/library-management/LibraryManagementMutations.svelte';
import type {
	LibraryManagementProfile,
	LibraryManagementProfileImportResponse,
	LibraryManagementRootAssignment,
	LibraryManagementRootOverrides,
	LibraryManagementSettings,
	LibraryManagementSettingsResponse,
	ManagementScriptSettings
} from '$lib/queries/library-management/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

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
		naming_script_id: null,
		multi_disc_naming_mode: 'inherit',
		multi_disc_naming_script_id: null
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

function findAssignment(
	settings: LibraryManagementSettings | undefined,
	rootId: string
): LibraryManagementRootAssignment {
	return (
		settings?.root_assignments.find((assignment) => assignment.root_id === rootId) ??
		emptyAssignment(rootId)
	);
}

export function createManagementSettings() {
	const query = getLibraryManagementSettingsQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const update = updateLibraryManagementSettingsMutation();
	const loaded = $derived(query.data ? settingsPayload(query.data) : undefined);

	let revision = $state('');
	let copyChoice = $state('');
	/** A failed check, copy or purge, shown on the save bar. */
	let error = $state('');

	const section = createSettingsSection<LibraryManagementSettings>(
		{
			get data() {
				return loaded;
			},
			get isPending() {
				return query.isPending;
			},
			get isError() {
				return query.isError;
			},
			get error() {
				return query.error;
			},
			refetch: () => query.refetch()
		},
		{
			mutateAsync: async (settings) => {
				const saved = await update.mutateAsync({
					settings,
					expected_settings_revision: revision
				});
				revision = saved.settings_revision;
				return settingsPayload(saved);
			},
			get isPending() {
				return update.isPending;
			}
		}
	);

	// Runs after the helper has taken the new data: an untouched draft now
	// holds it, so it carries its revision too.
	$effect.pre(() => {
		const latest = query.data?.settings_revision;
		if (latest !== undefined && !untrack(() => section.dirty)) revision = latest;
	});

	const profiles = $derived(section.draft?.profiles ?? []);

	function replaceDraft(next: LibraryManagementSettings): void {
		const draft = section.draft;
		if (draft) Object.assign(draft, structuredClone(next));
	}

	function updateAssignment(
		rootId: string,
		change: (assignment: LibraryManagementRootAssignment) => LibraryManagementRootAssignment
	): void {
		const draft = section.draft;
		if (!draft) return;
		const next = change($state.snapshot(findAssignment(draft, rootId)));
		const exists = draft.root_assignments.some((assignment) => assignment.root_id === rootId);
		draft.root_assignments = exists
			? draft.root_assignments.map((assignment) =>
					assignment.root_id === rootId ? next : assignment
				)
			: [...draft.root_assignments, next];
	}

	/** The roots that use a profile, by name or through the library default. */
	function rootLabelsFor(profileId: string, roots: LibraryRootSettings[]): string[] {
		const draft = section.draft;
		if (!draft) return [];
		return roots.flatMap((root) => {
			const assignment = draft.root_assignments.find((value) => value.root_id === root.id);
			return assignment && (assignment.profile_id ?? draft.default_profile_id) === profileId
				? [root.label]
				: [];
		});
	}

	return {
		section,
		query,
		get draft() {
			return section.draft;
		},
		get persisted() {
			return section.saved;
		},
		get dirty() {
			return section.dirty;
		},
		/** The revision the draft was loaded from; every write sends it. */
		get revision() {
			return revision;
		},
		get error() {
			return error;
		},
		set error(message: string) {
			error = message;
		},
		get profiles() {
			return profiles;
		},
		get defaultProfile() {
			return profiles.find((profile) => profile.id === section.draft?.default_profile_id) ?? null;
		},
		get activeAssignments() {
			return (section.saved?.root_assignments ?? []).filter(writesAutomatically);
		},
		/** The profile the copy row starts from: the last choice, else the default. */
		get copySourceProfileId() {
			return profiles.some((profile) => profile.id === copyChoice)
				? copyChoice
				: (section.draft?.default_profile_id ?? '');
		},
		set copySourceProfileId(id: string) {
			copyChoice = id;
		},

		/** A copy of the draft to send, or null before it loads. */
		snapshot(): LibraryManagementSettings | null {
			return section.draft ? ($state.snapshot(section.draft) as LibraryManagementSettings) : null;
		},
		replaceDraft,

		/** Takes settings the server saved outside the save bar (the activation wizard). */
		accept(saved: LibraryManagementSettingsResponse): void {
			revision = saved.settings_revision;
			replaceDraft(settingsPayload(saved));
		},

		/** Adds a profile the server just created by copying, keeping other edits. */
		acceptCopy(result: { profile: LibraryManagementProfile; settings_revision: string }): void {
			const draft = section.draft;
			if (!draft) return;
			draft.profiles = upsertProfile(draft.profiles, result.profile);
			revision = result.settings_revision;
		},

		acceptImport(result: LibraryManagementProfileImportResponse): void {
			const draft = section.draft;
			if (!draft) return;
			draft.profiles = upsertProfile(draft.profiles, result.profile);
			draft.naming_scripts = upsertScripts(draft.naming_scripts, result.naming_scripts);
			draft.tagging_scripts = upsertScripts(draft.tagging_scripts, result.tagging_scripts);
			revision = result.settings_revision;
			copyChoice = result.profile.id;
		},

		acceptDelete(saved: LibraryManagementSettingsResponse, deletedProfileId: string): void {
			const draft = section.draft;
			if (!draft) return;
			draft.profiles = draft.profiles.filter((profile) => profile.id !== deletedProfileId);
			revision = saved.settings_revision;
		},

		assignmentFor(rootId: string): LibraryManagementRootAssignment {
			return findAssignment(section.draft, rootId);
		},

		persistedAssignmentFor(rootId: string): LibraryManagementRootAssignment {
			return findAssignment(section.saved, rootId);
		},

		updateAssignment,

		updateOverrides(rootId: string, change: Partial<LibraryManagementRootOverrides>): void {
			updateAssignment(rootId, (assignment) => ({
				...assignment,
				overrides: { ...(assignment.overrides ?? emptyOverrides()), ...change }
			}));
		},

		setDefaultProfile(profileId: string): void {
			if (section.draft) section.draft.default_profile_id = profileId;
		},

		rootLabelsFor,

		/** Why a custom profile cannot be deleted, or null when it can. */
		deleteBlocker(profile: LibraryManagementProfile, roots: LibraryRootSettings[]): string | null {
			const draft = section.draft;
			if (!draft || profile.preset_origin) return null;
			if (profiles.length === 1) return 'Only remaining profile';
			if (profile.id === draft.default_profile_id) return 'Library default';
			if (rootLabelsFor(profile.id, roots).length > 0) return 'Assigned to root';
			return null;
		}
	};
}

export type ManagementSettings = ReturnType<typeof createManagementSettings>;
