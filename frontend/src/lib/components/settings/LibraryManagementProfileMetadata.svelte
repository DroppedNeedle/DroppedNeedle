<!-- Metadata fields: which tag fields the profile manages, and how, field by field. -->
<script lang="ts">
	import { ListFilter, Tags } from 'lucide-svelte';

	import LibraryManagementEditorSection from './LibraryManagementEditorSection.svelte';
	import type {
		GenreManagementSettings,
		ManagedFieldInfo,
		ManagementFieldMode,
		MetadataManagementSettings
	} from '$lib/queries/library-management/types';
	import Input from '$lib/ui/Input.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		metadata: MetadataManagementSettings;
		/** The genre row edits the Genres settings. */
		genres: GenreManagementSettings;
		/** Every tag field a profile manages, from the server. */
		managedFields: ManagedFieldInfo[];
	}

	let { metadata = $bindable(), genres = $bindable(), managedFields }: Props = $props();

	let fieldFilter = $state('');

	const modeLabels: Record<ManagementFieldMode, string> = {
		disabled: 'Off',
		replace: 'Replace',
		fill_missing: 'Fill missing',
		merge: 'Merge',
		preserve: 'Keep'
	};

	const visibleFields = $derived(
		managedFields.filter((field) => {
			const filter = fieldFilter.trim().toLowerCase();
			return field.name.includes(filter) || field.label.toLowerCase().includes(filter);
		})
	);
	const managedFieldCount = $derived(
		metadata.fields.filter((field) => field.mode !== 'disabled').length
	);

	function fieldEntry(name: string) {
		return metadata.fields.find((field) => field.field === name);
	}

	function fieldMode(name: string): ManagementFieldMode {
		if (name === 'genre') return genres.enabled ? genres.mode : 'disabled';
		return fieldEntry(name)?.mode ?? 'disabled';
	}

	function setFieldMode(name: string, mode: ManagementFieldMode) {
		if (name === 'genre') {
			genres.enabled = mode !== 'disabled';
			if (mode === 'replace' || mode === 'merge' || mode === 'fill_missing') {
				genres.mode = mode;
			}
			return;
		}
		const entry = fieldEntry(name);
		if (entry) {
			entry.mode = mode;
			if (mode !== 'replace') entry.clear_when_canonical_missing = false;
		} else {
			metadata.fields.push({ field: name, mode, clear_when_canonical_missing: false });
		}
	}

	function setFieldClear(name: string, clear: boolean) {
		const entry = fieldEntry(name);
		if (entry) entry.clear_when_canonical_missing = clear;
	}

	function sourceNote(field: ManagedFieldInfo): string {
		if (field.name === 'genre') return 'Set by the Genres section';
		// AcoustID values come from the file's audio, not MusicBrainz.
		if (field.name.startsWith('acoustid_') && field.source) return field.source;
		if (field.source) return `From MusicBrainz: ${field.source}`;
		return 'No source: kept, cleared, or set by a script';
	}
</script>

<LibraryManagementEditorSection
	icon={Tags}
	title="Metadata fields"
	subtitle="Choose authority field by field"
	active={metadata.enabled}
	chips={[
		{
			label: metadata.enabled
				? `${managedFieldCount} managed ${managedFieldCount === 1 ? 'field' : 'fields'}`
				: 'Saved choices retained'
		}
	]}
>
	<div class="mt-4 space-y-4">
		<ToggleRow
			label="Manage metadata tags"
			description="Off preserves all saved field choices for later."
			bind:checked={metadata.enabled}
		/>
		{#if metadata.enabled}
			<Input
				size="sm"
				icon={ListFilter}
				label="Filter managed fields"
				placeholder="Filter managed fields"
				bind:value={fieldFilter}
			/>
			<div class="management-field-table">
				{#each visibleFields as field (field.name)}
					{@const entry = fieldEntry(field.name)}
					<div class="management-field-row">
						<span class="flex min-w-0 flex-col">
							<span class="text-xs font-medium">{field.label}</span>
							<small class="text-[0.7rem] text-base-content/55">{sourceNote(field)}</small>
						</span>
						<select
							class="select select-sm"
							value={fieldMode(field.name)}
							onchange={(event) =>
								setFieldMode(field.name, event.currentTarget.value as ManagementFieldMode)}
							aria-label={`Mode for ${field.label}`}
						>
							{#each field.modes as mode (mode)}
								<option value={mode}>{modeLabels[mode]}</option>
							{/each}
							{#if fieldMode(field.name) === 'preserve'}
								<option value="preserve">{modeLabels.preserve}</option>
							{/if}
						</select>
						{#if field.clearable && entry?.mode === 'replace'}
							<span
								class="tooltip"
								data-tip="Clear this field when the selected release has no value"
							>
								<input
									type="checkbox"
									class="checkbox checkbox-sm"
									checked={entry.clear_when_canonical_missing}
									onchange={(event) => setFieldClear(field.name, event.currentTarget.checked)}
									aria-label={`Clear ${field.label} when the release has no value`}
								/>
							</span>
						{:else}
							<span aria-hidden="true"></span>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
	</div>
</LibraryManagementEditorSection>

<style>
	.management-field-table {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 15rem), 1fr));
		overflow: hidden;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 10%, transparent);
		border-radius: 0.75rem;
	}

	.management-field-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		min-width: 0;
		padding: 0.45rem 0.65rem;
		border-bottom: 1px solid color-mix(in oklab, var(--color-base-content) 7%, transparent);
		background: var(--color-base-100);
	}
</style>
