import {
	createPreviewLibraryPolicyImpactMutation,
	createSaveTargetLibrarySettingsMutation
} from '$lib/features/library-management/queries/LibraryPolicyMutations.svelte';
import { getTargetLibrarySettingsQuery } from '$lib/features/library-management/queries/LibraryPolicyQueries.svelte';
import type {
	TargetLibrarySettingsResponse,
	TypedLibrarySettings
} from '$lib/features/library-management/queries/LibraryOperationsTypes';
import { authStore } from '$lib/stores/authStore.svelte';
import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

function toForm(data: TargetLibrarySettingsResponse): TypedLibrarySettings {
	return {
		library_roots: data.library_roots.map((root) => ({
			...root,
			rules: root.rules.map((rule) => ({ ...rule }))
		})),
		staging_path: data.staging_path,
		naming_script: data.naming_script,
		acoustid_api_key: data.acoustid_api_key,
		enabled: data.enabled ?? true
	};
}

/**
 * The library folders and AcoustID form: its section, and the preview the
 * save goes through. Settings > Library shares one with the page around the
 * card. Call while a component initialises.
 */
export function createLibraryCard() {
	const settingsQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
	const impact = createPreviewLibraryPolicyImpactMutation();
	const save = createSaveTargetLibrarySettingsMutation();
	let savedSettings = $state<TargetLibrarySettingsResponse | null>(null);

	// Saving goes through a preview first; the save itself sends the revision
	// the preview was made against.
	const section = createSettingsSection<TypedLibrarySettings>(
		{
			get data() {
				return settingsQuery.data ? toForm(settingsQuery.data) : undefined;
			},
			get isPending() {
				return settingsQuery.isPending;
			},
			get isError() {
				return settingsQuery.isError;
			},
			get error() {
				return settingsQuery.error;
			},
			refetch: () => settingsQuery.refetch()
		},
		{
			mutateAsync: async (form) => {
				savedSettings = await save.mutateAsync({
					settings: form,
					expected_policy_revision: impact.data?.current_policy_revision ?? ''
				});
				return toForm(savedSettings);
			},
			get isPending() {
				return save.isPending;
			}
		}
	);
	const currentSettings = $derived(savedSettings ?? settingsQuery.data);

	return {
		settingsQuery,
		impact,
		save,
		section,
		/** The saved settings: a save's answer, else the query's. */
		get currentSettings() {
			return currentSettings;
		}
	};
}

export type LibraryCard = ReturnType<typeof createLibraryCard>;
