<!-- Artwork: the embedded front cover and the cover file beside the album. -->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { Image } from 'lucide-svelte';

	import LibraryManagementEditorAdvanced from './LibraryManagementEditorAdvanced.svelte';
	import LibraryManagementEditorSection from './LibraryManagementEditorSection.svelte';
	import { lines } from './managementProfileLists';
	import type { ArtworkManagementSettings } from '$lib/queries/library-management/types';
	import TextAreaField from '$lib/ui/TextAreaField.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		artwork: ArtworkManagementSettings;
	}

	let { artwork = $bindable() }: Props = $props();

	const outputFormats: Array<{
		value: ArtworkManagementSettings['embedded_format'];
		label: string;
	}> = [
		{ value: 'original', label: 'Original' },
		{ value: 'jpeg', label: 'JPEG' },
		{ value: 'png', label: 'PNG' },
		{ value: 'webp', label: 'WebP' }
	];

	const enabled = $derived(artwork.embedded_enabled || artwork.external_enabled);
</script>

<LibraryManagementEditorSection
	icon={Image}
	title="Artwork"
	subtitle="Embedded and external cover decisions"
	active={enabled}
	chips={[
		{
			label: artwork.embedded_enabled ? 'Embedded on' : 'Embedded off',
			active: artwork.embedded_enabled
		},
		{
			label: artwork.external_enabled ? 'External on' : 'External off',
			active: artwork.external_enabled
		}
	]}
>
	<div class="mt-4 grid gap-3 sm:grid-cols-2">
		<ToggleRow
			label="Embedded artwork"
			description="Embed the front cover in each file."
			bind:checked={artwork.embedded_enabled}
		/>
		<ToggleRow
			label="External artwork"
			description="Write a cover file beside the album."
			bind:checked={artwork.external_enabled}
		/>
		<LibraryManagementEditorAdvanced
			class="sm:col-span-2"
			title="Advanced artwork rules"
			subtitle="Sizes, formats, replacement and existing covers"
		>
			<div class="mt-3 grid gap-3 sm:grid-cols-2">
				<p class="sm:col-span-2 text-xs text-base-content/55">A size of 0 means no limit.</p>
				{#if artwork.embedded_enabled}
					<NumberField
						label="Embedded maximum size"
						min={0}
						bind:value={artwork.embedded_maximum_size}
					/>
					<SelectField
						label="Embedded format"
						options={outputFormats}
						bind:value={artwork.embedded_format}
					/>
				{/if}
				{#if artwork.external_enabled}
					<NumberField
						label="External maximum size"
						min={0}
						bind:value={artwork.external_maximum_size}
					/>
					<SelectField
						label="External format"
						options={outputFormats}
						bind:value={artwork.external_format}
					/>
				{/if}
				<ToggleRow
					label="Never replace with smaller"
					description="Compare image dimensions per file and type."
					bind:checked={artwork.never_replace_with_smaller}
				/>
				<div class="sm:col-span-2">
					<TextAreaField
						label="Existing cover files (one pattern per line)"
						mono
						rows={4}
						value={artwork.local_file_patterns.join('\n')}
						oninput={(text) => (artwork.local_file_patterns = lines(text))}
					/>
				</div>
			</div>
		</LibraryManagementEditorAdvanced>
	</div>
</LibraryManagementEditorSection>
