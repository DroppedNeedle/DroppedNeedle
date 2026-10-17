<!-- Credits and genres: artist naming, relationship credits and genre sources. -->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { Plus, Trash2, UsersRound } from 'lucide-svelte';

	import LibraryManagementEditorAdvanced from './LibraryManagementEditorAdvanced.svelte';
	import LibraryManagementEditorSection from './LibraryManagementEditorSection.svelte';
	import { lines, toggled } from './managementProfileLists';
	import type {
		GenreManagementSettings,
		MetadataManagementSettings
	} from '$lib/queries/library-management/types';
	import Button from '$lib/ui/Button.svelte';
	import Field from '$lib/ui/Field.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		metadata: MetadataManagementSettings;
		genres: GenreManagementSettings;
	}

	let { metadata = $bindable(), genres = $bindable() }: Props = $props();

	type RelationshipType = MetadataManagementSettings['relationships']['types'][number];
	type GenreSource = GenreManagementSettings['sources'][number];

	const relationshipTypes: Array<{ value: RelationshipType; label: string }> = [
		{ value: 'composer', label: 'Composer' },
		{ value: 'lyricist', label: 'Lyricist' },
		{ value: 'conductor', label: 'Conductor' },
		{ value: 'performer', label: 'Performer' },
		{ value: 'arranger', label: 'Arranger' },
		{ value: 'remixer', label: 'Remixer' },
		{ value: 'producer', label: 'Producer' },
		{ value: 'other', label: 'Other roles (engineer, mixer, DJ-mixer, writer, director)' }
	];
	const genreSources: Array<{ value: GenreSource; label: string }> = [
		{ value: 'musicbrainz', label: 'MusicBrainz' },
		{ value: 'listenbrainz', label: 'ListenBrainz' },
		{ value: 'lastfm', label: 'Last.fm' },
		{ value: 'existing_local', label: 'Existing local tags' }
	];

	const enabled = $derived(metadata.enabled || genres.enabled);
	const relationshipsOn = $derived(metadata.enabled && metadata.relationships.enabled);
</script>

<LibraryManagementEditorSection
	icon={UsersRound}
	title="Credits and genres"
	subtitle="Artist naming, relationships, translations, and genre sources"
	active={enabled}
	chips={[
		{
			label: metadata.enabled
				? `Credits · ${metadata.artist_credits.standardization}`
				: 'Credits inactive',
			active: metadata.enabled
		},
		{
			label: relationshipsOn
				? `Relationships · ${metadata.relationships.types.length}`
				: 'Relationships off',
			active: relationshipsOn
		},
		{
			label: genres.enabled ? `Genres · ${genres.mode.replace('_', ' ')}` : 'Genres off',
			active: genres.enabled
		}
	]}
>
	<div class="mt-4 grid gap-4 sm:grid-cols-2">
		<SelectField
			label="Artist credit style"
			options={[
				{ value: 'credited', label: 'Release credits' },
				{ value: 'canonical', label: 'Canonical names' }
			]}
			bind:value={metadata.artist_credits.standardization}
		/>
		<LibraryManagementEditorAdvanced
			class="sm:col-span-2"
			title="Artist name preferences"
			subtitle="Translations and preferred locales"
		>
			<div class="mt-3 grid gap-3">
				<ToggleRow
					label="Translate artist names"
					description="Use preferred locales when MusicBrainz supplies aliases."
					bind:checked={metadata.artist_credits.translate_names}
				/>
				<TextField
					label="Preferred artist locales"
					placeholder="en, en-GB, ja"
					bind:value={
						() => metadata.artist_credits.preferred_locales.join(', '),
						(value) =>
							(metadata.artist_credits.preferred_locales = value
								.split(',')
								.map((item) => item.trim())
								.filter(Boolean))
					}
				/>
			</div>
		</LibraryManagementEditorAdvanced>
		<div class="sm:col-span-2">
			<ToggleRow
				label="Relationship credits"
				description="Composer, performer, producer, and related roles."
				bind:checked={metadata.relationships.enabled}
			/>
		</div>
		{#if metadata.relationships.enabled}
			<LibraryManagementEditorAdvanced
				class="sm:col-span-2"
				title="Relationship roles"
				subtitle={`${metadata.relationships.types.length} selected`}
			>
				<div class="management-choice-grid mt-3" aria-label="Relationship credit types">
					{#each relationshipTypes as relationship (relationship.value)}
						<label>
							<input
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={metadata.relationships.types.includes(relationship.value)}
								onchange={(event) =>
									(metadata.relationships.types = toggled(
										metadata.relationships.types,
										relationship.value,
										event.currentTarget.checked
									))}
							/>
							<span>{relationship.label}</span>
						</label>
					{/each}
				</div>
			</LibraryManagementEditorAdvanced>
		{/if}
		<div class="sm:col-span-2">
			<ToggleRow
				label="Manage genres"
				description="Source thresholds and saved lists remain intact while off."
				bind:checked={genres.enabled}
			/>
		</div>
		{#if genres.enabled}
			<div class="management-choice-grid sm:col-span-2" aria-label="Genre sources">
				{#each genreSources as source (source.value)}
					<label>
						<input
							type="checkbox"
							class="checkbox checkbox-sm"
							checked={genres.sources.includes(source.value)}
							onchange={(event) =>
								(genres.sources = toggled(
									genres.sources,
									source.value,
									event.currentTarget.checked
								))}
						/>
						<span>{source.label}</span>
					</label>
				{/each}
			</div>
			<SelectField
				label="Genre behavior"
				options={[
					{ value: 'replace', label: 'Replace' },
					{ value: 'merge', label: 'Merge' },
					{ value: 'fill_missing', label: 'Fill missing' }
				]}
				bind:value={genres.mode}
			/>
			<NumberField label="Maximum genres" min={1} max={50} bind:value={genres.maximum_count} />
			<LibraryManagementEditorAdvanced
				class="sm:col-span-2"
				title="Advanced genre rules"
				subtitle="Thresholds, filtering, casing, and aliases"
			>
				<div class="mt-3 grid gap-3 sm:grid-cols-2">
					<NumberField
						label="MusicBrainz minimum votes"
						min={0}
						bind:value={genres.musicbrainz_minimum_count}
					/>
					<NumberField
						label="ListenBrainz minimum votes"
						min={0}
						bind:value={genres.listenbrainz_minimum_count}
					/>
					<NumberField
						label="Last.fm minimum weight"
						min={0}
						bind:value={genres.lastfm_minimum_weight}
					/>
					<NumberField
						label="Maximum ancestry depth"
						min={0}
						max={20}
						bind:value={genres.maximum_ancestry_depth}
					/>
					<ToggleRow
						label="Curated ListenBrainz tags only"
						description="Reject uncurated community tags."
						bind:checked={genres.listenbrainz_curated_only}
					/>
					<ToggleRow
						label="Allowlisted Last.fm tags only"
						description="Require a configured accepted genre."
						bind:checked={genres.lastfm_whitelist_only}
					/>
					<ToggleRow
						label="Canonicalize genres"
						description="Apply aliases and ancestry."
						bind:checked={genres.canonicalize}
					/>
					<ToggleRow
						label="Primary genre on constrained formats"
						description="Use one value where multi-values are lossy."
						bind:checked={genres.write_primary_only_for_constrained_formats}
					/>
					<Field label="Genre allowlist (one per line)">
						{#snippet children(control)}
							<textarea
								{...control}
								class="textarea min-h-24"
								value={genres.allowlist.join('\n')}
								oninput={(event) => (genres.allowlist = lines(event.currentTarget.value))}
							></textarea>
						{/snippet}
					</Field>
					<Field label="Genre denylist (one per line)">
						{#snippet children(control)}
							<textarea
								{...control}
								class="textarea min-h-24"
								value={genres.denylist.join('\n')}
								oninput={(event) => (genres.denylist = lines(event.currentTarget.value))}
							></textarea>
						{/snippet}
					</Field>
					<div class="sm:col-span-2">
						<Field label="Preferred casing (one per line)">
							{#snippet children(control)}
								<textarea
									{...control}
									class="textarea min-h-20"
									value={genres.preferred_casing.join('\n')}
									oninput={(event) => (genres.preferred_casing = lines(event.currentTarget.value))}
								></textarea>
							{/snippet}
						</Field>
					</div>
					<div class="space-y-2 sm:col-span-2">
						<div class="flex items-center justify-between">
							<strong class="text-sm">Genre aliases</strong>
							<Button
								variant="ghost"
								size="xs"
								icon={Plus}
								onclick={() => (genres.aliases = [...genres.aliases, { source: '', target: '' }])}
								>Add alias</Button
							>
						</div>
						{#each genres.aliases as alias, index (`${index}:${alias.source}`)}
							<div class="grid grid-cols-[1fr_auto_1fr_auto] items-center gap-2">
								<Input
									size="sm"
									label={`Genre alias source ${index + 1}`}
									bind:value={alias.source}
								/>
								<span aria-hidden="true">→</span>
								<Input
									size="sm"
									label={`Genre alias target ${index + 1}`}
									bind:value={alias.target}
								/>
								<IconButton
									variant="danger-ghost"
									size="xs"
									shape="square"
									icon={Trash2}
									label={`Remove genre alias ${index + 1}`}
									onclick={() =>
										(genres.aliases = genres.aliases.filter(
											(_, valueIndex) => valueIndex !== index
										))}
								/>
							</div>
						{/each}
					</div>
				</div>
			</LibraryManagementEditorAdvanced>
		{/if}
	</div>
</LibraryManagementEditorSection>
