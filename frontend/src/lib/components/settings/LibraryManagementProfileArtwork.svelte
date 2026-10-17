<!-- Artwork: embedded and external cover art, its sources and its rules. -->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { ArrowDown, ArrowUp, Image, X } from 'lucide-svelte';

	import LibraryManagementEditorAdvanced from './LibraryManagementEditorAdvanced.svelte';
	import LibraryManagementEditorSection from './LibraryManagementEditorSection.svelte';
	import { lines, toggled } from './managementProfileLists';
	import type {
		ArtworkImageType,
		ArtworkManagementSettings,
		ArtworkProvider,
		ManagementScriptSettings
	} from '$lib/queries/library-management/types';
	import Card from '$lib/ui/Card.svelte';
	import Field from '$lib/ui/Field.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		artwork: ArtworkManagementSettings;
		/** The naming scripts, offered for external artwork file names. */
		namingScripts: ManagementScriptSettings[];
	}

	let { artwork = $bindable(), namingScripts }: Props = $props();

	const artworkProviders: Array<{ value: ArtworkProvider; label: string }> = [
		{ value: 'cover_art_archive_release', label: 'Exact-release artwork' },
		{ value: 'cover_art_archive_release_group', label: 'Release-group fallback' },
		{ value: 'local_files', label: 'Local files' },
		{ value: 'embedded', label: 'Existing embedded art' }
	];
	const artworkTypes: Array<{ value: ArtworkImageType; label: string }> = [
		{ value: 'front', label: 'Front cover' },
		{ value: 'back', label: 'Back cover' },
		{ value: 'booklet', label: 'Booklet' },
		{ value: 'medium', label: 'Media label' },
		{ value: 'tray', label: 'Tray insert' },
		{ value: 'obi', label: 'Obi strip' },
		{ value: 'spine', label: 'Spine' },
		{ value: 'track', label: 'Track artwork' },
		{ value: 'other', label: 'Other artwork' }
	];
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

	function addProvider(value: string): void {
		const provider = value as ArtworkProvider;
		if (!provider || artwork.providers.includes(provider)) return;
		artwork.providers = [...artwork.providers, provider];
	}

	function moveProvider(index: number, direction: -1 | 1): void {
		const target = index + direction;
		if (target < 0 || target >= artwork.providers.length) return;
		const providers = [...artwork.providers];
		[providers[index], providers[target]] = [providers[target], providers[index]];
		artwork.providers = providers;
	}

	function providerLabel(provider: ArtworkProvider): string {
		return artworkProviders.find((option) => option.value === provider)?.label ?? provider;
	}
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
			description="Write selected images into supported audio containers."
			bind:checked={artwork.embedded_enabled}
		/>
		<ToggleRow
			label="External artwork"
			description="Create named image files beside the album."
			bind:checked={artwork.external_enabled}
		/>
		<Card as="section" variant="bordered" padding="sm" class="sm:col-span-2">
			<div class="flex items-end justify-between gap-3">
				<div>
					<strong class="text-sm">Artwork source priority</strong>
					<p class="text-xs text-base-content/55">Sources are tried from top to bottom.</p>
				</div>
				<select
					class="select select-sm"
					aria-label="Add artwork source"
					value=""
					onchange={(event) => {
						addProvider(event.currentTarget.value);
						event.currentTarget.value = '';
					}}
				>
					<option value="">Add source…</option>
					{#each artworkProviders.filter((provider) => !artwork.providers.includes(provider.value)) as provider (provider.value)}
						<option value={provider.value}>{provider.label}</option>
					{/each}
				</select>
			</div>
			<ol class="mt-1 grid gap-2" aria-label="Artwork source priority">
				{#each artwork.providers as provider, index (provider)}
					<li
						class="flex items-center gap-2 rounded-lg border border-base-content/10 bg-base-100 px-3 py-2"
					>
						<span class="badge badge-ghost badge-sm font-mono">{index + 1}</span>
						<span class="min-w-0 flex-1 text-sm">{providerLabel(provider)}</span>
						<IconButton
							size="xs"
							shape="square"
							icon={ArrowUp}
							label={`Move ${providerLabel(provider)} earlier`}
							disabled={index === 0}
							onclick={() => moveProvider(index, -1)}
						/>
						<IconButton
							size="xs"
							shape="square"
							icon={ArrowDown}
							label={`Move ${providerLabel(provider)} later`}
							disabled={index === artwork.providers.length - 1}
							onclick={() => moveProvider(index, 1)}
						/>
						<IconButton
							variant="danger-ghost"
							size="xs"
							shape="square"
							icon={X}
							label={`Remove ${providerLabel(provider)}`}
							onclick={() =>
								(artwork.providers = artwork.providers.filter((value) => value !== provider))}
						/>
					</li>
				{/each}
			</ol>
		</Card>
		<div class="management-choice-grid sm:col-span-2" aria-label="Artwork image types">
			{#each artworkTypes as imageType (imageType.value)}
				<label
					><input
						type="checkbox"
						class="checkbox checkbox-sm"
						checked={artwork.image_types.includes(imageType.value)}
						onchange={(event) =>
							(artwork.image_types = toggled(
								artwork.image_types,
								imageType.value,
								event.currentTarget.checked
							))}
					/><span>{imageType.label}</span></label
				>
			{/each}
		</div>
		<LibraryManagementEditorAdvanced
			class="sm:col-span-2"
			title="Advanced artwork rules"
			subtitle="Dimensions, formats, replacement, and naming"
		>
			<div class="mt-3 grid gap-3 sm:grid-cols-2">
				<p class="sm:col-span-2 text-xs text-base-content/55">
					A size of 0 means unlimited. Minimum dimensions filter candidates; maximum sizes resize
					outputs when needed.
				</p>
				<NumberField label="Minimum width" min={0} bind:value={artwork.minimum_width} />
				<NumberField label="Minimum height" min={0} bind:value={artwork.minimum_height} />
				<SelectField
					label="Download size"
					options={[
						{ value: 'full', label: 'Full size' },
						{ value: '1200', label: '1200 px' },
						{ value: '500', label: '500 px' },
						{ value: '250', label: '250 px' }
					]}
					bind:value={artwork.download_size}
				/>
				<ToggleRow
					label="Approved cover art only"
					description="Require provider approval where available."
					bind:checked={artwork.approved_only}
				/>
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
					<ToggleRow
						label="Embed front art only"
						description="Do not embed additional image types."
						bind:checked={artwork.embedded_front_only}
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
					<ToggleRow
						label="External front art only"
						description="Do not create other image types."
						bind:checked={artwork.external_front_only}
					/>
					<ToggleRow
						label="Overwrite external artwork"
						description="Leaves existing files untouched and reports naming collisions."
						bind:checked={artwork.overwrite_external_files}
					/>
				{/if}
				<ToggleRow
					label="Never replace with smaller"
					description="Compare image dimensions per file and type."
					bind:checked={artwork.never_replace_with_smaller}
				/>
				<Field label="Local file patterns (one per line)">
					{#snippet children(control)}
						<textarea
							{...control}
							class="textarea min-h-24"
							value={artwork.local_file_patterns.join('\n')}
							oninput={(event) => (artwork.local_file_patterns = lines(event.currentTarget.value))}
						></textarea>
					{/snippet}
				</Field>
				<fieldset class="grid gap-2 text-sm">
					<legend class="mb-1">Preserve existing image types</legend>
					{#each artworkTypes as imageType (imageType.value)}
						<label class="flex items-center gap-2 text-xs"
							><input
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={artwork.preserve_existing_types.includes(imageType.value)}
								onchange={(event) =>
									(artwork.preserve_existing_types = toggled(
										artwork.preserve_existing_types,
										imageType.value,
										event.currentTarget.checked
									))}
							/><span>{imageType.label}</span></label
						>
					{/each}
				</fieldset>
				{#if artwork.external_enabled}
					<div class="sm:col-span-2">
						<SelectField
							label="External artwork naming script"
							options={[
								{ value: '', label: 'Default album filenames (cover.jpg, back.jpg, …)' },
								...namingScripts.map((script) => ({ value: script.id, label: script.name }))
							]}
							bind:value={
								() => artwork.external_naming_script_id ?? '',
								(id) => (artwork.external_naming_script_id = id || null)
							}
						/>
					</div>
				{/if}
			</div>
		</LibraryManagementEditorAdvanced>
	</div>
</LibraryManagementEditorSection>
