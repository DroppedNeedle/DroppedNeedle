<!--
	The management profile editor. It holds the draft profile and the script
	lists, and saves or discards them; each section edits its own part of the
	draft through a binding.
-->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { onMount } from 'svelte';
	import { RotateCcw, ShieldCheck, Tags, X } from 'lucide-svelte';

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
	import { getLibraryManagementPresetDiffQuery } from '$lib/queries/library-management/LibraryManagementQueries.svelte';
	import type {
		LibraryManagementProfile,
		ManagedFieldInfo,
		ManagementScriptSettings
	} from '$lib/queries/library-management/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

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
	let dialog: HTMLDialogElement;
	let heading: HTMLHeadingElement;
	let resetDialog: HTMLDialogElement;
	let resetHeading: HTMLHeadingElement;
	let resetOpener: HTMLElement | null = null;
	let discardDialog: HTMLDialogElement;
	let discardHeading: HTMLHeadingElement;
	let closeOpener: HTMLElement | null = null;
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

	onMount(() => {
		dialog.showModal();
		heading.focus();
	});

	function requestReset(group: PresetGroup, opener: HTMLElement): void {
		resetGroup = group;
		resetOpener = opener;
		resetDialog.showModal();
		resetHeading.focus();
	}

	function restoreResetFocus(): void {
		resetOpener?.focus();
		resetOpener = null;
		resetGroup = null;
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
		resetDialog.close();
	}

	function requestClose(opener: HTMLElement | null = null): void {
		if (saving) return;
		if (!dirty) {
			dialog.close();
			return;
		}
		closeOpener = opener;
		discardDialog.showModal();
		discardHeading.focus();
	}

	function restoreCloseFocus(): void {
		closeOpener?.focus();
		closeOpener = null;
	}

	function discardChanges(): void {
		discardDialog.close();
		dialog.close();
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

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby="management-profile-title"
	{onclose}
	oncancel={(event) => {
		event.preventDefault();
		requestClose();
	}}
>
	<div class="modal-box management-profile-editor max-w-5xl p-0">
		<header class="management-profile-editor__header">
			<div class="min-w-0">
				<p class="management-kicker"><Tags class="h-3.5 w-3.5" /> Management profile</p>
				<h2
					bind:this={heading}
					id="management-profile-title"
					tabindex="-1"
					class="font-display text-2xl font-semibold"
				>
					{draft.name}
				</h2>
				<p class="mt-1 text-sm text-base-content/55">
					Controls what DroppedNeedle may write, rename, and move. Editing never enables a root.
				</p>
			</div>
			<IconButton
				label="Close profile editor"
				icon={X}
				size="sm"
				shape="square"
				disabled={saving}
				onclick={(event: MouseEvent & { currentTarget: HTMLElement }) =>
					requestClose(event.currentTarget)}
			/>
		</header>

		<div class="max-h-[72vh] space-y-4 overflow-y-auto p-5 sm:p-6">
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
			<LibraryManagementProfileArtwork bind:artwork={draft.artwork} namingScripts={scripts} />
			<LibraryManagementProfileNaming bind:organization={draft.organization} bind:scripts />
			<LibraryManagementProfileSafety
				bind:metadata={draft.metadata}
				bind:fileBehavior={draft.file_behavior}
				bind:identity={draft.identity}
				bind:notification={draft.notification}
			/>

			{#if localError}<div class="alert alert-error text-sm" role="alert">{localError}</div>{/if}
		</div>

		<footer class="management-profile-editor__footer">
			<div class="flex min-w-0 items-center gap-2">
				<span class="management-editor-state" data-active={dirty}
					>{dirty ? 'Unsaved' : 'Saved'}</span
				>
				<p class="text-xs text-base-content/45">
					{dirty
						? 'Saving validates the full profile. If it expands write access, active roots need a new dry run.'
						: 'This profile has no unsaved changes.'}
				</p>
			</div>
			<div class="flex gap-2">
				<Button
					variant="ghost"
					disabled={saving}
					onclick={(event: MouseEvent & { currentTarget: HTMLElement }) =>
						requestClose(event.currentTarget)}>Cancel</Button
				>
				<Button variant="warning" loading={saving} disabled={!dirty} onclick={() => void save()}
					>Save profile</Button
				>
			</div>
		</footer>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button
			aria-label="Close profile editor"
			disabled={saving}
			onclick={(event) => {
				event.preventDefault();
				requestClose();
			}}>close</button
		>
	</form>
</dialog>

<dialog
	bind:this={resetDialog}
	class="modal"
	aria-labelledby="management-profile-reset-title"
	onclose={restoreResetFocus}
>
	<div class="modal-box max-w-md">
		<p class="management-kicker"><RotateCcw class="h-3.5 w-3.5" /> Preset values</p>
		<h2
			bind:this={resetHeading}
			id="management-profile-reset-title"
			tabindex="-1"
			class="font-display text-xl font-semibold"
		>
			Reset {resetGroup ? presetGroupLabels[resetGroup] : 'section'}?
		</h2>
		<p class="mt-3 text-sm text-base-content/65">
			This replaces only this section in the editor. Review the values, then save the profile to
			apply the reset. No files or root settings are changed here.
		</p>
		<div class="modal-action">
			<Button variant="ghost" onclick={() => resetDialog.close()}>Cancel</Button>
			<Button variant="warning" onclick={resetSection}>Reset section</Button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Cancel preset reset">close</button>
	</form>
</dialog>

<dialog
	bind:this={discardDialog}
	class="modal"
	aria-labelledby="management-profile-discard-title"
	onclose={restoreCloseFocus}
>
	<div class="modal-box max-w-md">
		<p class="management-kicker"><ShieldCheck class="h-3.5 w-3.5" /> Unsaved profile</p>
		<h2
			bind:this={discardHeading}
			id="management-profile-discard-title"
			tabindex="-1"
			class="font-display text-xl font-semibold"
		>
			Discard your changes?
		</h2>
		<p class="mt-3 text-sm text-base-content/65">
			The profile has unsaved changes. Closing now will discard them.
		</p>
		<div class="modal-action">
			<Button variant="ghost" onclick={() => discardDialog.close()}>Keep editing</Button>
			<Button variant="danger" onclick={discardChanges}>Discard changes</Button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close discard confirmation">close</button>
	</form>
</dialog>

<style>
	.management-profile-editor {
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 24%, transparent);
		background:
			linear-gradient(
				145deg,
				color-mix(in oklab, var(--color-library-manage) 5%, var(--color-base-100)),
				var(--color-base-100) 18rem
			),
			var(--color-base-100);
	}
</style>
