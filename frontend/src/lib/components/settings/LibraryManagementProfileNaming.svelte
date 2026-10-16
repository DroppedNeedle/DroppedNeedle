<!-- File naming and organization: rename, move, sidecars, path rules and cleanup. -->
<script lang="ts">
	import { FolderCog } from 'lucide-svelte';

	import LibraryManagementEditorAdvanced from './LibraryManagementEditorAdvanced.svelte';
	import LibraryManagementEditorSection from './LibraryManagementEditorSection.svelte';
	import LibraryManagementScriptEditor from './LibraryManagementScriptEditor.svelte';
	import { lines } from './managementProfileLists';
	import type {
		ManagementScriptSettings,
		OrganizationManagementSettings
	} from '$lib/queries/library-management/types';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		organization: OrganizationManagementSettings;
		/** Every naming script, shared by all profiles. */
		scripts: ManagementScriptSettings[];
	}

	let { organization = $bindable(), scripts = $bindable() }: Props = $props();

	const enabled = $derived(
		organization.rename_enabled || organization.move_enabled || organization.move_sidecars
	);
	const scriptOptions = $derived(
		scripts.map((script) => ({ value: script.id, label: script.name }))
	);
</script>

<LibraryManagementEditorSection
	icon={FolderCog}
	title="File naming and organization"
	subtitle="Paths, sidecars, and source cleanup"
	active={enabled}
	chips={[
		{
			label: organization.rename_enabled ? 'Rename on' : 'Rename off',
			active: organization.rename_enabled
		},
		{
			label: organization.move_enabled ? 'Move on' : 'Move off',
			active: organization.move_enabled
		},
		{
			label: organization.move_sidecars ? 'Sidecars on' : 'Sidecars off',
			active: organization.move_sidecars
		}
	]}
>
	<div class="mt-4 space-y-4">
		<div class="grid gap-3 sm:grid-cols-3">
			<ToggleRow
				label="Rename"
				description="Render the naming script."
				bind:checked={organization.rename_enabled}
			/>
			<ToggleRow
				label="Move"
				description="Organize within the root."
				bind:checked={organization.move_enabled}
			/>
			<ToggleRow
				label="Sidecars"
				description="Move matched album files."
				bind:checked={organization.move_sidecars}
			/>
		</div>
		<p class="text-xs font-semibold uppercase tracking-wider text-base-content/55">
			Scripts used by this profile
		</p>
		<div class="mb-3 grid gap-3 sm:grid-cols-2">
			<SelectField
				label="Single-disc naming script"
				options={scriptOptions}
				bind:value={organization.naming_script_id}
			/>
			<SelectField
				label="Multi-disc naming script"
				options={[{ value: '', label: 'Use the single-disc script' }, ...scriptOptions]}
				bind:value={
					() => organization.multi_disc_naming_script_id ?? '',
					(id) => (organization.multi_disc_naming_script_id = id || null)
				}
			/>
		</div>
		<p class="mb-3 text-xs text-base-content/60">
			Releases with more than one MusicBrainz audio medium use the multi-disc script.
		</p>
		<LibraryManagementScriptEditor
			kind="naming"
			{scripts}
			selectedIds={[
				organization.naming_script_id,
				...(organization.multi_disc_naming_script_id
					? [organization.multi_disc_naming_script_id]
					: [])
			]}
			onchange={(value) => (scripts = value)}
		/>
		<LibraryManagementEditorAdvanced
			title="Path compatibility"
			subtitle="Platform-safe names and path limits"
		>
			<div class="mt-3 grid gap-3 sm:grid-cols-2">
				<ToggleRow
					label="Windows-compatible names"
					description="Apply reserved-character and device-name rules on every host."
					bind:checked={organization.compatibility.windows_compatible}
				/>
				<ToggleRow
					label="Legacy Windows path limit"
					description="Keep the absolute path within 259 characters."
					bind:checked={organization.compatibility.windows_legacy_path_limit}
				/>
				<ToggleRow
					label="Replace non-ASCII"
					description="Use compatibility transliteration."
					bind:checked={organization.compatibility.replace_non_ascii}
				/>
				<ToggleRow
					label="Spaces to underscores"
					description="Applies after rendering."
					bind:checked={organization.compatibility.replace_spaces_with_underscores}
				/>
				<Field
					label="Invalid-character replacement"
					help="Replaces path separators and characters unsafe for the selected platform rules."
				>
					{#snippet children(control)}
						<Input
							{...control}
							label="Invalid-character replacement"
							maxlength={8}
							bind:value={organization.compatibility.separator_replacement}
						/>
					{/snippet}
				</Field>
				<SelectField
					label="Unicode normalization"
					help="NFC preserves character distinctions; NFKC also folds compatibility forms."
					options={[
						{ value: 'NFC', label: 'NFC' },
						{ value: 'NFKC', label: 'NFKC' }
					]}
					bind:value={organization.compatibility.unicode_normalization}
				/>
				<NumberField
					label="Maximum component length"
					help="Longer folder or file names are shortened safely."
					min={1}
					bind:value={organization.compatibility.maximum_component_length}
				/>
				<NumberField
					label="Maximum path length"
					help="A dry run blocks paths that still exceed this full-path limit."
					min={1}
					bind:value={organization.compatibility.maximum_path_length}
				/>
				<div class="sm:col-span-2">
					<SelectField
						label="Extension case"
						help="Changes only the filename extension, never the audio format."
						options={[
							{ value: 'preserve', label: 'Preserve' },
							{ value: 'lower', label: 'Lowercase' },
							{ value: 'upper', label: 'Uppercase' }
						]}
						bind:value={organization.compatibility.extension_case}
					/>
				</div>
			</div>
		</LibraryManagementEditorAdvanced>
		<LibraryManagementEditorAdvanced
			title="Sidecars and source cleanup"
			subtitle="Matched files and verified post-move cleanup"
		>
			<div class="mt-3 grid gap-3 sm:grid-cols-2">
				<div class="sm:col-span-2">
					<Field label="Sidecar patterns (one per line)">
						{#snippet children(control)}
							<textarea
								{...control}
								class="textarea min-h-24 font-mono text-xs"
								value={organization.sidecar_patterns.join('\n')}
								oninput={(event) =>
									(organization.sidecar_patterns = lines(event.currentTarget.value))}></textarea>
						{/snippet}
					</Field>
				</div>
				<SelectField
					label="Source after confirmed move"
					help="Removal happens only after publication and catalog commit are verified."
					options={[
						{ value: 'keep', label: 'Keep source' },
						{ value: 'remove_after_confirmed_move', label: 'Remove verified source' }
					]}
					bind:value={organization.source_cleanup}
				/>
				<div class="sm:mt-6">
					<ToggleRow
						label="Remove empty directories"
						description="Only after verified cleanup."
						bind:checked={organization.remove_empty_directories}
					/>
				</div>
			</div>
		</LibraryManagementEditorAdvanced>
	</div>
</LibraryManagementEditorSection>
