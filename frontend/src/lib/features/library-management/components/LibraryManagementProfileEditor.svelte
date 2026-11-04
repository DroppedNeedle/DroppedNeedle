<!--
	The management profile editor. It holds the draft profile and the script
	lists, and saves or discards them; each section edits its own part of the
	draft through a binding.
-->
<script lang="ts">
	import '$lib/features/library-management/components/library-management.css';
	import { RotateCcw } from 'lucide-svelte';

	import LibraryManagementProfileArtwork from './LibraryManagementProfileArtwork.svelte';
	import LibraryManagementProfileCredits from './LibraryManagementProfileCredits.svelte';
	import LibraryManagementProfileLyrics from './LibraryManagementProfileLyrics.svelte';
	import LibraryManagementProfileMetadata from './LibraryManagementProfileMetadata.svelte';
	import LibraryManagementProfileName, {
		presetGroupLabels,
		type PresetGroup
	} from './LibraryManagementProfileName.svelte';
	import LibraryManagementProfileNaming from './LibraryManagementProfileNaming.svelte';
	import LibraryManagementProfileSafety from './LibraryManagementProfileSafety.svelte';
	import LibraryManagementProfileTagScripts from './LibraryManagementProfileTagScripts.svelte';
	import { getLibraryManagementPresetDiffQuery } from '$lib/features/library-management/queries/LibraryManagementQueries.svelte';
	import type {
		LibraryManagementProfile,
		ManagedFieldInfo,
		ManagementScriptSettings
	} from '$lib/features/library-management/queries/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import Modal from '$lib/ui/Modal.svelte';

	interface Props {
		profile: LibraryManagementProfile;
		/** Every tag field a profile manages, from the server. */
		managedFields: ManagedFieldInfo[];
		namingScripts: ManagementScriptSettings[];
		taggingScripts: ManagementScriptSettings[];
		saving?: boolean;
		onclose: () => void;
		onsave: (
			profile: LibraryManagementProfile,
			namingScripts: ManagementScriptSettings[],
			taggingScripts: ManagementScriptSettings[]
		) => Promise<void>;
	}

	let {
		profile,
		managedFields,
		namingScripts,
		taggingScripts,
		saving = false,
		onclose,
		onsave
	}: Props = $props();
	let shown = $state(true);
	let resetOpen = $state(false);
	let discardOpen = $state(false);
	const initialProfile = (): LibraryManagementProfile => structuredClone($state.snapshot(profile));
	const initialScripts = (): ManagementScriptSettings[] =>
		structuredClone($state.snapshot(namingScripts));
	const initialTaggingScripts = (): ManagementScriptSettings[] =>
		structuredClone($state.snapshot(taggingScripts));
	const originalProfile = initialProfile();
	const originalScripts = initialScripts();
	const originalTaggingScripts = initialTaggingScripts();
	let draft = $state<LibraryManagementProfile>(structuredClone(originalProfile));
	let scripts = $state<ManagementScriptSettings[]>(structuredClone(originalScripts));
	let tagScripts = $state<ManagementScriptSettings[]>(structuredClone(originalTaggingScripts));
	let localError = $state('');
	let resetGroup = $state<PresetGroup | null>(null);
	const presetGroups = Object.keys(presetGroupLabels) as PresetGroup[];
	const presetDiffQuery = getLibraryManagementPresetDiffQuery(
		() => authStore.user?.id,
		() => (profile.preset_origin ? profile.id : null)
	);
	const changedPresetGroups = $derived.by(() => {
		const preset = presetDiffQuery.data?.preset_profile;
		if (!preset) return [];
		return presetGroups.filter(
			(group) => JSON.stringify(draft[group]) !== JSON.stringify(preset[group])
		);
	});
	const dirty = $derived(
		JSON.stringify(draft) !== JSON.stringify(originalProfile) ||
			JSON.stringify(scripts) !== JSON.stringify(originalScripts) ||
			JSON.stringify(tagScripts) !== JSON.stringify(originalTaggingScripts)
	);

	function requestReset(group: PresetGroup): void {
		resetGroup = group;
		resetOpen = true;
	}

	function copyGroup<K extends PresetGroup>(group: K, preset: LibraryManagementProfile): void {
		draft[group] = structuredClone(preset[group]);
	}

	function resetSection(): void {
		const presetDiff = presetDiffQuery.data;
		const preset = presetDiff?.preset_profile;
		if (!preset || !resetGroup) return;
		copyGroup(resetGroup, preset);
		if (presetDiff.version_upgrade_groups.includes(resetGroup)) {
			draft.preset_version = preset.preset_version;
		}
	}

	function close(): void {
		shown = false;
		onclose();
	}

	function requestClose(): void {
		if (saving) return;
		if (dirty) discardOpen = true;
		else close();
	}

	async function save(): Promise<void> {
		localError = '';
		if (!draft.name.trim()) {
			localError = 'Give this profile a name.';
			return;
		}
		try {
			await onsave($state.snapshot(draft), $state.snapshot(scripts), $state.snapshot(tagScripts));
		} catch (error) {
			localError = error instanceof Error ? error.message : 'Could not save this profile.';
		}
	}
</script>

<Modal
	bind:open={shown}
	size="sheet"
	title={draft.name}
	description="Controls what DroppedNeedle may write, rename, and move. Editing never enables a root."
	focusTitle
	dismissible={!saving}
	onrequestclose={requestClose}
>
	<div class="space-y-4 p-4 md:h-full md:overflow-y-auto md:p-6">
		<LibraryManagementProfileName
			bind:draft
			changedGroups={presetDiffQuery.data ? changedPresetGroups : null}
			compareFailed={presetDiffQuery.isError}
			onreset={requestReset}
		/>
		<LibraryManagementProfileMetadata
			bind:metadata={draft.metadata}
			bind:genres={draft.genres}
			{managedFields}
		/>
		<LibraryManagementProfileLyrics bind:enrichment={draft.enrichment} />
		<LibraryManagementProfileTagScripts
			bind:scripts={tagScripts}
			bind:selectedIds={draft.metadata.tagging_script_ids}
		/>
		<LibraryManagementProfileCredits bind:metadata={draft.metadata} bind:genres={draft.genres} />
		<LibraryManagementProfileArtwork bind:artwork={draft.artwork} />
		<LibraryManagementProfileNaming bind:organization={draft.organization} bind:scripts />
		<LibraryManagementProfileSafety
			bind:metadata={draft.metadata}
			bind:identity={draft.identity}
			bind:notification={draft.notification}
		/>

		{#if localError}<div class="alert alert-error text-sm" role="alert">{localError}</div>{/if}
	</div>
	{#snippet actions()}
		<div class="flex flex-wrap items-center justify-between gap-2">
			<div class="flex min-w-0 items-center gap-2">
				<span class="management-editor-state" data-active={dirty}
					>{dirty ? 'Unsaved' : 'Saved'}</span
				>
				<p class="text-xs text-muted">
					{dirty
						? 'Saving validates the full profile. If it expands write access, active roots need a new dry run.'
						: 'This profile has no unsaved changes.'}
				</p>
			</div>
			<div class="ml-auto flex gap-2">
				<Button variant="ghost" disabled={saving} onclick={requestClose}>Cancel</Button>
				<Button variant="warning" loading={saving} disabled={!dirty} onclick={() => void save()}
					>Save profile</Button
				>
			</div>
		</div>
	{/snippet}
</Modal>

<ConfirmDialog
	bind:open={resetOpen}
	title="Reset {resetGroup ? presetGroupLabels[resetGroup] : 'section'}?"
	message="This replaces only this section in the editor. Review the values, then save the profile to apply the reset. No files or root settings are changed here."
	confirmLabel="Reset section"
	tone="warning"
	icon={RotateCcw}
	focusTitle
	onconfirm={resetSection}
	onclose={() => (resetGroup = null)}
/>

<ConfirmDialog
	bind:open={discardOpen}
	title="Discard your changes?"
	message="The profile has unsaved changes. Closing now will discard them."
	confirmLabel="Discard changes"
	cancelLabel="Keep editing"
	tone="danger"
	focusTitle
	onconfirm={close}
/>
