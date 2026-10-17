<!--
	One library root on the Automation tab: its profile, whether file
	organization is set up there, the automatic triggers and the per-root
	overrides of the profile's values. Edits go into the tab's draft.
-->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import type { LibraryRootSettings } from '$lib/queries/library/LibraryOperationsTypes';
	import type { LibraryManagementRootOverrides } from '$lib/queries/library-management/types';
	import Card from '$lib/ui/Card.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	import LibraryManagementProfilePicker from './LibraryManagementProfilePicker.svelte';
	import LibraryManagementRootPlacement from './LibraryManagementRootPlacement.svelte';
	import { emptyOverrides, type ManagementSettingsDraft } from './managementSettingsDraft.svelte';

	interface Props {
		root: LibraryRootSettings;
		settings: ManagementSettingsDraft;
		/** One line on whether management is on, pending or paused here. */
		status: string;
	}

	let { root, settings, status }: Props = $props();

	type BooleanOverrideKey =
		| 'metadata_enabled'
		| 'genres_enabled'
		| 'embedded_artwork_enabled'
		| 'external_artwork_enabled'
		| 'rename_enabled'
		| 'move_enabled'
		| 'move_sidecars'
		| 'preserve_timestamps';
	const booleanOverrides: Array<{ key: BooleanOverrideKey; label: string }> = [
		{ key: 'metadata_enabled', label: 'Metadata tags' },
		{ key: 'genres_enabled', label: 'Genres' },
		{ key: 'embedded_artwork_enabled', label: 'Embedded artwork' },
		{ key: 'external_artwork_enabled', label: 'External artwork' },
		{ key: 'rename_enabled', label: 'Rename files' },
		{ key: 'move_enabled', label: 'Organize within this root' },
		{ key: 'move_sidecars', label: 'Move sidecars' },
		{ key: 'preserve_timestamps', label: 'Preserve timestamps' }
	];
	const booleanChoices: Array<{ value: '' | 'true' | 'false'; label: string }> = [
		{ value: '', label: 'Inherit profile' },
		{ value: 'true', label: 'On' },
		{ value: 'false', label: 'Off' }
	];
	const multiDiscChoices: Array<{
		value: LibraryManagementRootOverrides['multi_disc_naming_mode'];
		label: string;
	}> = [
		{ value: 'inherit', label: 'Inherit profile' },
		{ value: 'standard', label: 'Use effective single-disc script' },
		{ value: 'script', label: 'Use selected script' }
	];
	const sourceCleanupChoices: Array<{
		value: '' | 'keep' | 'remove_after_confirmed_move';
		label: string;
	}> = [
		{ value: '', label: 'Inherit profile' },
		{ value: 'keep', label: 'Keep source' },
		{ value: 'remove_after_confirmed_move', label: 'Remove after verified move' }
	];

	const assignment = $derived(settings.assignmentFor(root.id));
	const namingScripts = $derived(settings.draft?.naming_scripts ?? []);

	function update(change: (value: typeof assignment) => typeof assignment): void {
		settings.updateAssignment(root.id, change);
	}

	function updateOverrides(change: Partial<LibraryManagementRootOverrides>): void {
		settings.updateOverrides(root.id, change);
	}

	function nullableBoolean(value: string): boolean | null {
		return value === '' ? null : value === 'true';
	}

	function setMultiDiscMode(mode: LibraryManagementRootOverrides['multi_disc_naming_mode']): void {
		updateOverrides({
			multi_disc_naming_mode: mode,
			multi_disc_naming_script_id:
				mode === 'script'
					? (assignment.overrides?.multi_disc_naming_script_id ?? namingScripts[0]?.id ?? null)
					: null
		});
	}
</script>

<Card as="article" variant="bordered" padding="md">
	<div class="flex flex-wrap items-start justify-between gap-3">
		<div>
			<div class="flex flex-wrap items-center gap-2">
				<h4 class="font-display text-lg font-semibold">{root.label}</h4>
				<span class="badge badge-ghost badge-sm"
					>Scanning: {root.policy === 'automatic'
						? 'Automatic identification'
						: root.policy.replace('_', ' ')}</span
				>
			</div>
			<p class="mt-1 font-mono text-xs text-base-content/40">{root.path}</p>
		</div>
		<span class="management-status-badge" data-active={assignment.enabled}>{status}</span>
	</div>
	<LibraryManagementRootPlacement rootId={root.id} rootLabel={root.label} />
	<div class="mt-4">
		<LibraryManagementProfilePicker
			id={`root-${root.id}`}
			eyebrow={assignment.profile_id ? 'Explicit root profile' : 'Inherited from library default'}
			label={`Management profile for ${root.label}`}
			description="Defines tags, artwork, enrichment, and file organization for this root. Choose the library default or make an explicit override."
			profiles={settings.profiles}
			selectedId={assignment.profile_id ?? ''}
			changed={assignment.profile_id !== settings.persistedAssignmentFor(root.id).profile_id}
			inheritedProfile={settings.defaultProfile}
			onselect={(profileId) => update((value) => ({ ...value, profile_id: profileId || null }))}
		/>
	</div>
	<div class="mt-4 border-t border-base-content/10 pt-4">
		<div class="max-w-xl">
			<ToggleRow
				label="Configure file organization for this root"
				description="This alone starts nothing. Choose and confirm automatic triggers below."
				bind:checked={
					() => assignment.enabled, (enabled) => update((value) => ({ ...value, enabled }))
				}
			/>
		</div>
	</div>
	<div class="mt-3 grid gap-2 md:grid-cols-3" aria-label={`Automatic triggers for ${root.label}`}>
		<label class="management-trigger"
			><input
				type="checkbox"
				class="checkbox checkbox-sm"
				disabled={!assignment.enabled}
				checked={assignment.automatic_acquisitions}
				onchange={(event) =>
					update((value) => ({
						...value,
						automatic_acquisitions: event.currentTarget.checked
					}))}
			/><span
				><strong>Acquisitions</strong><small>Applies to new Soulseek and Usenet downloads.</small
				></span
			></label
		>
		<label class="management-trigger"
			><input
				type="checkbox"
				class="checkbox checkbox-sm"
				disabled={!assignment.enabled}
				checked={assignment.automatic_drop_imports}
				onchange={(event) =>
					update((value) => ({
						...value,
						automatic_drop_imports: event.currentTarget.checked
					}))}
			/><span
				><strong>Drop & Free imports</strong><small>Only identified, mapped files.</small></span
			></label
		>
		<label class="management-trigger"
			><input
				type="checkbox"
				class="checkbox checkbox-sm"
				disabled={!assignment.enabled}
				checked={assignment.automatic_scan_discovered}
				onchange={(event) =>
					update((value) => ({
						...value,
						automatic_scan_discovered: event.currentTarget.checked,
						automatic_custom_editions: event.currentTarget.checked
							? value.automatic_custom_editions
							: false
					}))}
			/><span
				><strong>Scan-discovered</strong><small
					>Off by default. Accepted release and track identity required.</small
				></span
			></label
		>
		<label class="management-trigger pl-6"
			><input
				type="checkbox"
				class="checkbox checkbox-sm"
				disabled={!assignment.enabled || !assignment.automatic_scan_discovered}
				checked={assignment.automatic_custom_editions}
				onchange={(event) =>
					update((value) => ({
						...value,
						automatic_custom_editions: event.currentTarget.checked
					}))}
			/><span
				><strong>Include Custom editions</strong><small
					>Requires a current sealed manifest and a fresh dry run.</small
				></span
			></label
		>
	</div>
	<details class="mt-3 rounded-xl border border-base-content/10 p-3">
		<summary class="cursor-pointer text-xs font-semibold">Per-root profile overrides</summary>
		<div class="mt-3 max-w-xl">
			<ToggleRow
				label="Override selected profile values"
				description="Every untouched control continues to inherit the profile."
				bind:checked={
					() => assignment.overrides !== null,
					(on) => update((value) => ({ ...value, overrides: on ? emptyOverrides() : null }))
				}
			/>
		</div>
		{#if assignment.overrides}
			{@const overrides = assignment.overrides}
			<div class="mt-3 grid gap-x-3 sm:grid-cols-2 lg:grid-cols-3">
				{#each booleanOverrides as control (control.key)}
					<SelectField
						label={control.label}
						options={booleanChoices}
						bind:value={
							() =>
								overrides[control.key] === null
									? ''
									: (String(overrides[control.key]) as 'true' | 'false'),
							(choice) => updateOverrides({ [control.key]: nullableBoolean(choice) })
						}
					/>
				{/each}
				<SelectField
					label="Naming script"
					options={[
						{ value: '', label: 'Inherit profile' },
						...namingScripts.map((script) => ({ value: script.id, label: script.name }))
					]}
					bind:value={
						() => overrides.naming_script_id ?? '',
						(id) => updateOverrides({ naming_script_id: id || null })
					}
				/>
				<SelectField
					label="Multi-disc naming"
					options={multiDiscChoices}
					bind:value={() => overrides.multi_disc_naming_mode, setMultiDiscMode}
				/>
				{#if overrides.multi_disc_naming_mode === 'script'}
					<SelectField
						label="Selected multi-disc script"
						options={namingScripts.map((script) => ({ value: script.id, label: script.name }))}
						bind:value={
							() => overrides.multi_disc_naming_script_id ?? '',
							(id) => updateOverrides({ multi_disc_naming_script_id: id || null })
						}
					/>
				{/if}
				<SelectField
					label="Source cleanup"
					options={sourceCleanupChoices}
					bind:value={
						() => overrides.source_cleanup ?? '',
						(choice) => updateOverrides({ source_cleanup: choice || null })
					}
				/>
			</div>
		{/if}
	</details>
</Card>
