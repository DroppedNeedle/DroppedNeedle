<script lang="ts">
	import { onMount } from 'svelte';
	import {
		AudioWaveform,
		ArrowDown,
		ArrowUp,
		Braces,
		Check,
		ChevronRight,
		FolderCog,
		Image,
		ListFilter,
		Plus,
		RotateCcw,
		ShieldCheck,
		Tags,
		Trash2,
		UsersRound,
		X
	} from 'lucide-svelte';

	import LibraryManagementScriptEditor from './LibraryManagementScriptEditor.svelte';
	import { getLibraryManagementPresetDiffQuery } from '$lib/queries/library-management/LibraryManagementQueries.svelte';
	import type {
		ArtworkImageType,
		ArtworkProvider,
		LibraryManagementProfile,
		ManagementScriptSettings
	} from '$lib/queries/library-management/types';
	import { authStore } from '$lib/stores/authStore.svelte';

	interface Props {
		profile: LibraryManagementProfile;
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

	let { profile, namingScripts, taggingScripts, saving = false, onclose, onsave }: Props = $props();
	let dialog: HTMLDialogElement;
	let heading: HTMLHeadingElement;
	let resetDialog: HTMLDialogElement;
	let resetHeading: HTMLHeadingElement;
	let resetOpener: HTMLButtonElement | null = null;
	let discardDialog: HTMLDialogElement;
	let discardHeading: HTMLHeadingElement;
	let closeOpener: HTMLButtonElement | null = null;
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
	let fieldFilter = $state('');
	let localError = $state('');
	type PresetGroup =
		| 'metadata'
		| 'genres'
		| 'artwork'
		| 'organization'
		| 'file_behavior'
		| 'enrichment'
		| 'notification';
	let resetGroup = $state<PresetGroup | null>(null);
	const presetGroups: PresetGroup[] = [
		'metadata',
		'genres',
		'artwork',
		'organization',
		'file_behavior',
		'enrichment',
		'notification'
	];
	const presetGroupLabels: Record<PresetGroup, string> = {
		metadata: 'Metadata',
		genres: 'Genres',
		artwork: 'Artwork',
		organization: 'File organization',
		file_behavior: 'File safety',
		enrichment: 'Lyrics and ReplayGain',
		notification: 'Notifications'
	};
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
	type RelationshipType = LibraryManagementProfile['metadata']['relationships']['types'][number];
	type GenreSource = LibraryManagementProfile['genres']['sources'][number];

	const relationshipTypes: Array<{ value: RelationshipType; label: string }> = [
		{ value: 'composer', label: 'Composer' },
		{ value: 'lyricist', label: 'Lyricist' },
		{ value: 'conductor', label: 'Conductor' },
		{ value: 'performer', label: 'Performer' },
		{ value: 'arranger', label: 'Arranger' },
		{ value: 'remixer', label: 'Remixer' },
		{ value: 'producer', label: 'Producer' },
		{ value: 'other', label: 'Other relationships' }
	];
	const genreSources: Array<{ value: GenreSource; label: string }> = [
		{ value: 'musicbrainz', label: 'MusicBrainz' },
		{ value: 'listenbrainz', label: 'ListenBrainz' },
		{ value: 'lastfm', label: 'Last.fm' },
		{ value: 'existing_local', label: 'Existing local tags' }
	];
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
	const mergeableFields = new Set([
		'artist',
		'album_artist',
		'artist_sort',
		'album_artist_sort',
		'release_type',
		'label',
		'catalog_number',
		'isrc',
		'musicbrainz_artist_id',
		'musicbrainz_album_artist_id',
		'musicbrainz_work_id',
		'composer',
		'lyricist',
		'conductor',
		'performer',
		'arranger',
		'remixer',
		'producer'
	]);

	const visibleFields = $derived(
		draft.metadata.fields.filter((field) =>
			field.field.toLowerCase().includes(fieldFilter.trim().toLowerCase())
		)
	);
	const managedFieldCount = $derived(
		draft.metadata.fields.filter((field) => field.mode !== 'disabled').length
	);
	const enrichmentEnabled = $derived(
		draft.enrichment.lyrics.enabled || draft.enrichment.replaygain.enabled
	);
	const lyricsOutputs = $derived.by(() => {
		if (!draft.enrichment.lyrics.enabled) return 'Lyrics off';
		const outputs: string[] = [];
		if (draft.enrichment.lyrics.write_synced) outputs.push('synced');
		if (draft.enrichment.lyrics.write_plain) outputs.push('plain');
		return outputs.length > 0 ? `Lyrics · ${outputs.join(' + ')}` : 'Lyrics · no output';
	});
	const replayGainSummary = $derived(
		draft.enrichment.replaygain.enabled
			? `ReplayGain · ${draft.enrichment.replaygain.mode.replace('_', ' ')}`
			: 'ReplayGain off'
	);
	const creditsAndGenresEnabled = $derived(draft.metadata.enabled || draft.genres.enabled);
	const artistCreditSummary = $derived(
		draft.metadata.enabled
			? `Credits · ${draft.metadata.artist_credits.standardization}`
			: 'Credits inactive'
	);
	const relationshipSummary = $derived(
		draft.metadata.enabled && draft.metadata.relationships.enabled
			? `Relationships · ${draft.metadata.relationships.types.length}`
			: 'Relationships off'
	);
	const genreSummary = $derived(
		draft.genres.enabled ? `Genres · ${draft.genres.mode.replace('_', ' ')}` : 'Genres off'
	);
	const artworkEnabled = $derived(draft.artwork.embedded_enabled || draft.artwork.external_enabled);
	const organizationEnabled = $derived(
		draft.organization.rename_enabled ||
			draft.organization.move_enabled ||
			draft.organization.move_sidecars
	);
	const enabledSafeguardCount = $derived(
		[
			draft.file_behavior.preserve_timestamps,
			draft.file_behavior.preserve_permissions,
			draft.file_behavior.strict_capability_gate,
			draft.file_behavior.reject_symlinks,
			draft.file_behavior.validate_written_metadata,
			draft.file_behavior.validate_technical_audio
		].filter(Boolean).length
	);

	onMount(() => {
		dialog.showModal();
		heading.focus();
	});

	function updateNamingScripts(value: ManagementScriptSettings[]): void {
		scripts = value;
	}

	function updateTaggingScripts(value: ManagementScriptSettings[], selectedIds: string[]): void {
		tagScripts = value;
		draft.metadata.tagging_script_ids = selectedIds;
	}

	function toggled<T>(values: T[], value: T, checked: boolean): T[] {
		return checked
			? values.includes(value)
				? values
				: [...values, value]
			: values.filter((item) => item !== value);
	}

	function lines(value: string): string[] {
		return value
			.split('\n')
			.map((item) => item.trim())
			.filter(Boolean);
	}

	function addGenreAlias(): void {
		draft.genres.aliases = [...draft.genres.aliases, { source: '', target: '' }];
	}

	function addArtworkProvider(value: string): void {
		const provider = value as ArtworkProvider;
		if (!provider || draft.artwork.providers.includes(provider)) return;
		draft.artwork.providers = [...draft.artwork.providers, provider];
	}

	function moveArtworkProvider(index: number, direction: -1 | 1): void {
		const target = index + direction;
		if (target < 0 || target >= draft.artwork.providers.length) return;
		const providers = [...draft.artwork.providers];
		[providers[index], providers[target]] = [providers[target], providers[index]];
		draft.artwork.providers = providers;
	}

	function artworkProviderLabel(provider: ArtworkProvider): string {
		return artworkProviders.find((option) => option.value === provider)?.label ?? provider;
	}

	function requestReset(group: PresetGroup, opener: HTMLButtonElement): void {
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

	function resetSection(): void {
		const presetDiff = presetDiffQuery.data;
		const preset = presetDiff?.preset_profile;
		if (!preset || !resetGroup) return;
		switch (resetGroup) {
			case 'metadata':
				draft.metadata = structuredClone(preset.metadata);
				break;
			case 'genres':
				draft.genres = structuredClone(preset.genres);
				break;
			case 'artwork':
				draft.artwork = structuredClone(preset.artwork);
				break;
			case 'organization':
				draft.organization = structuredClone(preset.organization);
				break;
			case 'file_behavior':
				draft.file_behavior = structuredClone(preset.file_behavior);
				break;
			case 'enrichment':
				draft.enrichment = structuredClone(preset.enrichment);
				break;
			case 'notification':
				draft.notification = structuredClone(preset.notification);
		}
		if (presetDiff.version_upgrade_groups.includes(resetGroup)) {
			draft.preset_version = preset.preset_version;
		}
		resetDialog.close();
	}

	function requestClose(opener: HTMLButtonElement | null = null): void {
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
			<button
				class="btn btn-ghost btn-sm btn-square"
				aria-label="Close profile editor"
				disabled={saving}
				onclick={(event) => requestClose(event.currentTarget)}
			>
				<X class="h-5 w-5" />
			</button>
		</header>

		<div class="max-h-[72vh] space-y-4 overflow-y-auto p-5 sm:p-6">
			<section class="management-editor-section grid gap-4 lg:grid-cols-2">
				<label class="grid gap-1.5">
					<span class="text-xs font-semibold uppercase tracking-wider text-base-content/55"
						>Name</span
					>
					<input class="input input-bordered bg-base-100" bind:value={draft.name} />
				</label>
				<label class="grid gap-1.5">
					<span class="text-xs font-semibold uppercase tracking-wider text-base-content/55"
						>Description</span
					>
					<input class="input input-bordered bg-base-100" bind:value={draft.description} />
				</label>
				<div class="lg:col-span-2 flex flex-wrap gap-2 text-xs">
					<span class="badge badge-outline">{draft.preset_origin ? 'Preset based' : 'Custom'}</span>
					<span class="badge badge-ghost font-mono">rev {draft.revision.slice(0, 8)}</span>
				</div>
				{#if draft.preset_origin && presetDiffQuery.data}<div
						class="lg:col-span-2 rounded-xl border border-base-content/10 bg-base-200/40 p-3 text-sm"
					>
						<strong
							>{changedPresetGroups.length > 0
								? 'Customized from preset'
								: 'Matches saved preset'}</strong
						>
						<p class="mt-1 text-xs text-base-content/55">
							{changedPresetGroups.length > 0
								? `Changed sections: ${changedPresetGroups.map((group) => presetGroupLabels[group].toLowerCase()).join(', ')}`
								: 'No profile sections differ from its recorded preset version.'}
						</p>
						{#if changedPresetGroups.length > 0 && presetDiffQuery.data.preset_profile}
							<div class="mt-3 flex flex-wrap gap-2">
								{#each changedPresetGroups as group (group)}
									<button
										class="btn btn-ghost btn-xs"
										onclick={(event) => requestReset(group, event.currentTarget)}
									>
										<RotateCcw class="h-3.5 w-3.5" /> Reset {presetGroupLabels[group]}
									</button>
								{/each}
							</div>
						{/if}
					</div>{:else if draft.preset_origin && presetDiffQuery.isError}<div
						class="alert alert-error lg:col-span-2 py-2 text-xs"
						role="alert"
					>
						Could not compare this profile with its preset. Saved values are unchanged.
					</div>{/if}
			</section>

			<details class="management-editor-section" data-active={draft.metadata.enabled}>
				<summary class="management-editor-summary">
					<span class="management-editor-icon"><Tags class="h-4 w-4" /></span>
					<span class="management-editor-summary__copy"
						><strong>Metadata fields</strong><small>Choose authority field by field</small></span
					>
					<span class="management-editor-overview">
						<span class="management-editor-state" data-active={draft.metadata.enabled}
							>{draft.metadata.enabled ? 'On' : 'Off'}</span
						>
						<span class="management-editor-chip"
							>{draft.metadata.enabled
								? `${managedFieldCount} managed ${managedFieldCount === 1 ? 'field' : 'fields'}`
								: 'Saved choices retained'}</span
						>
					</span>
					<ChevronRight class="h-4 w-4 management-editor-chevron" />
				</summary>
				<div class="mt-4 space-y-4">
					<label class="management-master-toggle">
						<input type="checkbox" class="toggle toggle-sm" bind:checked={draft.metadata.enabled} />
						<span
							><strong>Manage metadata tags</strong><small
								>Off preserves all saved field choices for later.</small
							></span
						>
					</label>
					{#if draft.metadata.enabled}
						<label class="input input-sm input-bordered flex items-center gap-2 bg-base-100">
							<ListFilter class="h-4 w-4 text-base-content/40" />
							<input class="grow" bind:value={fieldFilter} placeholder="Filter managed fields" />
						</label>
						<div class="management-field-table">
							{#each visibleFields as field (field.field)}
								<label class="management-field-row">
									<span class="min-w-0 font-mono text-xs">{field.field.replaceAll('_', ' ')}</span>
									<select
										class="select select-ghost select-xs"
										bind:value={field.mode}
										aria-label={`Mode for ${field.field}`}
									>
										<option value="disabled">Off</option>
										<option value="replace">Replace</option>
										<option value="fill_missing">Fill missing</option>
										{#if mergeableFields.has(field.field)}<option value="merge">Merge</option>{/if}
									</select>
									{#if field.mode === 'replace'}
										<span
											class="tooltip"
											data-tip="Clear this field when the selected release has no value"
										>
											<input
												type="checkbox"
												class="checkbox checkbox-xs"
												bind:checked={field.clear_when_canonical_missing}
												aria-label={`Clear ${field.field} when canonical value is missing`}
											/>
										</span>
									{:else}
										<span aria-hidden="true"></span>
									{/if}
								</label>
							{/each}
						</div>
					{/if}
				</div>
			</details>

			<details class="management-editor-section" data-active={enrichmentEnabled}>
				<summary class="management-editor-summary">
					<span class="management-editor-icon"><AudioWaveform class="h-4 w-4" /></span>
					<span class="management-editor-summary__copy"
						><strong>Lyrics and loudness</strong><small
							>Optional LRCLIB lyrics and ReplayGain analysis</small
						></span
					>
					<span class="management-editor-overview">
						<span class="management-editor-state" data-active={enrichmentEnabled}
							>{enrichmentEnabled ? 'On' : 'Off'}</span
						>
						<span class="management-editor-chip" data-active={draft.enrichment.lyrics.enabled}
							>{lyricsOutputs}</span
						>
						<span class="management-editor-chip" data-active={draft.enrichment.replaygain.enabled}
							>{replayGainSummary}</span
						>
					</span>
					<ChevronRight class="h-4 w-4 management-editor-chevron" />
				</summary>
				<div class="mt-4 grid gap-4 lg:grid-cols-2">
					<section class="rounded-xl border border-base-content/10 bg-base-100/35 p-4">
						<label class="management-master-toggle">
							<input
								type="checkbox"
								class="toggle toggle-sm"
								bind:checked={draft.enrichment.lyrics.enabled}
							/>
							<span
								><strong>Fetch lyrics from LRCLIB</strong><small
									>Exact title, artist, album, and duration matches only.</small
								></span
							>
						</label>
						<fieldset class="mt-4 grid gap-3" disabled={!draft.enrichment.lyrics.enabled}>
							<label class="management-master-toggle">
								<input
									type="checkbox"
									class="checkbox checkbox-sm"
									bind:checked={draft.enrichment.lyrics.write_plain}
								/>
								<span
									><strong>Write plain lyrics</strong><small
										>Uses Picard-compatible lyrics tags in every admitted format.</small
									></span
								>
							</label>
							<label class="management-master-toggle">
								<input
									type="checkbox"
									class="checkbox checkbox-sm"
									bind:checked={draft.enrichment.lyrics.write_synced}
								/>
								<span
									><strong>Write synchronized lyrics</strong><small
										>Preferred when safely supported; plain lyrics remain the fallback.</small
									></span
								>
							</label>
							<details class="management-editor-advanced">
								<summary>
									<span
										><strong>Advanced lyrics behavior</strong><small
											>Preservation and safety gates</small
										></span
									>
									<ChevronRight class="h-4 w-4" />
								</summary>
								<div class="mt-3 grid gap-3">
									<label class="management-master-toggle">
										<input
											type="checkbox"
											class="checkbox checkbox-sm"
											bind:checked={draft.enrichment.lyrics.preserve_existing}
										/>
										<span
											><strong>Preserve existing lyrics</strong><small
												>Fill only empty selected lyrics fields instead of replacing them.</small
											></span
										>
									</label>
									<label class="management-master-toggle">
										<input
											type="checkbox"
											class="checkbox checkbox-sm"
											bind:checked={draft.enrichment.lyrics.required}
										/>
										<span
											><strong>Require lyrics</strong><small
												>Hold the release when no selected lyrics format has an exact match.</small
											></span
										>
									</label>
								</div>
							</details>
						</fieldset>
					</section>

					<section class="rounded-xl border border-base-content/10 bg-base-100/35 p-4">
						<label class="management-master-toggle">
							<input
								type="checkbox"
								class="toggle toggle-sm"
								bind:checked={draft.enrichment.replaygain.enabled}
							/>
							<span
								><strong>Manage ReplayGain</strong><small
									>Analyze loudness without changing audio samples.</small
								></span
							>
						</label>
						<fieldset class="mt-4 grid gap-3" disabled={!draft.enrichment.replaygain.enabled}>
							<label class="grid gap-1.5 text-sm">
								<span>Existing ReplayGain values</span>
								<select
									class="select select-bordered bg-base-100"
									bind:value={draft.enrichment.replaygain.mode}
								>
									<option value="preserve">Preserve</option>
									<option value="fill_missing">Fill missing</option>
									<option value="replace">Replace</option>
								</select>
								<p class="text-xs text-base-content/55">
									Preserve keeps existing ReplayGain tags and performs no loudness analysis. Fill
									missing and Replace run analysis.
								</p>
							</label>
							<details class="management-editor-advanced">
								<summary>
									<span
										><strong>Advanced analysis behavior</strong><small
											>Album coherence and safety gates</small
										></span
									>
									<ChevronRight class="h-4 w-4" />
								</summary>
								<div class="mt-3 grid gap-3">
									<label class="management-master-toggle">
										<input
											type="checkbox"
											class="checkbox checkbox-sm"
											bind:checked={draft.enrichment.replaygain.album_aware}
										/>
										<span
											><strong>Album-aware analysis</strong><small
												>Calculate coherent track and album gain/peak values.</small
											></span
										>
									</label>
									<label class="management-master-toggle">
										<input
											type="checkbox"
											class="checkbox checkbox-sm"
											bind:checked={draft.enrichment.replaygain.required}
										/>
										<span
											><strong>Require ReplayGain</strong><small
												>Hold the whole unit when the selected values are unavailable.</small
											></span
										>
									</label>
								</div>
							</details>
						</fieldset>
					</section>
				</div>
			</details>

			<details
				class="management-editor-section"
				data-active={draft.metadata.tagging_script_ids.length > 0}
			>
				<summary class="management-editor-summary">
					<span class="management-editor-icon"><Braces class="h-4 w-4" /></span>
					<span class="management-editor-summary__copy"
						><strong>Tag transformations</strong><small
							>Ordered metadata scripts, separate from file naming</small
						></span
					>
					<span class="management-editor-overview">
						<span
							class="management-editor-state"
							data-active={draft.metadata.tagging_script_ids.length > 0}
							>{draft.metadata.tagging_script_ids.length > 0 ? 'On' : 'Off'}</span
						>
						<span class="management-editor-chip">
							{draft.metadata.tagging_script_ids.length > 0
								? `${draft.metadata.tagging_script_ids.length} attached ${draft.metadata.tagging_script_ids.length === 1 ? 'script' : 'scripts'}`
								: 'No scripts attached'}
						</span>
					</span>
					<ChevronRight class="h-4 w-4 management-editor-chevron" />
				</summary>
				<div class="mt-4">
					<LibraryManagementScriptEditor
						kind="tagging"
						scripts={tagScripts}
						selectedIds={draft.metadata.tagging_script_ids}
						onchange={updateTaggingScripts}
					/>
				</div>
			</details>

			<details class="management-editor-section" data-active={creditsAndGenresEnabled}>
				<summary class="management-editor-summary">
					<span class="management-editor-icon"><UsersRound class="h-4 w-4" /></span>
					<span class="management-editor-summary__copy"
						><strong>Credits and genres</strong><small
							>Artist naming, relationships, translations, and genre sources</small
						></span
					>
					<span class="management-editor-overview">
						<span class="management-editor-state" data-active={creditsAndGenresEnabled}
							>{creditsAndGenresEnabled ? 'On' : 'Off'}</span
						>
						<span class="management-editor-chip" data-active={draft.metadata.enabled}
							>{artistCreditSummary}</span
						>
						<span
							class="management-editor-chip"
							data-active={draft.metadata.enabled && draft.metadata.relationships.enabled}
							>{relationshipSummary}</span
						>
						<span class="management-editor-chip" data-active={draft.genres.enabled}
							>{genreSummary}</span
						>
					</span>
					<ChevronRight class="h-4 w-4 management-editor-chevron" />
				</summary>
				<div class="mt-4 grid gap-4 sm:grid-cols-2">
					<label class="grid gap-1.5 text-sm">
						<span>Artist credit style</span>
						<select
							class="select select-bordered bg-base-100"
							bind:value={draft.metadata.artist_credits.standardization}
						>
							<option value="credited">Release credits</option>
							<option value="canonical">Canonical names</option>
						</select>
					</label>
					<details class="management-editor-advanced sm:col-span-2">
						<summary>
							<span
								><strong>Artist name preferences</strong><small
									>Translations and preferred locales</small
								></span
							>
							<ChevronRight class="h-4 w-4" />
						</summary>
						<div class="mt-3 grid gap-3 sm:grid-cols-2">
							<label class="management-master-toggle sm:col-span-2">
								<input
									type="checkbox"
									class="toggle toggle-sm"
									bind:checked={draft.metadata.artist_credits.translate_names}
								/>
								<span
									><strong>Translate artist names</strong><small
										>Use preferred locales when MusicBrainz supplies aliases.</small
									></span
								>
							</label>
							<label class="grid gap-1.5 text-sm sm:col-span-2">
								<span>Preferred artist locales</span>
								<input
									class="input input-bordered bg-base-100"
									value={draft.metadata.artist_credits.preferred_locales.join(', ')}
									oninput={(event) =>
										(draft.metadata.artist_credits.preferred_locales = event.currentTarget.value
											.split(',')
											.map((item) => item.trim())
											.filter(Boolean))}
									placeholder="en, en-GB, ja"
								/>
							</label>
						</div>
					</details>
					<label class="management-master-toggle sm:col-span-2">
						<input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.metadata.relationships.enabled}
						/>
						<span
							><strong>Relationship credits</strong><small
								>Composer, performer, producer, and related roles.</small
							></span
						>
					</label>
					{#if draft.metadata.relationships.enabled}
						<details class="management-editor-advanced sm:col-span-2">
							<summary>
								<span
									><strong>Relationship roles</strong><small
										>{draft.metadata.relationships.types.length} selected</small
									></span
								>
								<ChevronRight class="h-4 w-4" />
							</summary>
							<div class="management-choice-grid mt-3" aria-label="Relationship credit types">
								{#each relationshipTypes as relationship (relationship.value)}
									<label>
										<input
											type="checkbox"
											class="checkbox checkbox-xs"
											checked={draft.metadata.relationships.types.includes(relationship.value)}
											onchange={(event) =>
												(draft.metadata.relationships.types = toggled(
													draft.metadata.relationships.types,
													relationship.value,
													event.currentTarget.checked
												))}
										/>
										<span>{relationship.label}</span>
									</label>
								{/each}
							</div>
						</details>
					{/if}
					<label class="management-master-toggle sm:col-span-2">
						<input type="checkbox" class="toggle toggle-sm" bind:checked={draft.genres.enabled} />
						<span
							><strong>Manage genres</strong><small
								>Source thresholds and saved lists remain intact while off.</small
							></span
						>
					</label>
					{#if draft.genres.enabled}
						<div class="management-choice-grid sm:col-span-2" aria-label="Genre sources">
							{#each genreSources as source (source.value)}
								<label>
									<input
										type="checkbox"
										class="checkbox checkbox-xs"
										checked={draft.genres.sources.includes(source.value)}
										onchange={(event) =>
											(draft.genres.sources = toggled(
												draft.genres.sources,
												source.value,
												event.currentTarget.checked
											))}
									/>
									<span>{source.label}</span>
								</label>
							{/each}
						</div>
						<label class="grid gap-1.5 text-sm">
							<span>Genre behavior</span>
							<select class="select select-bordered bg-base-100" bind:value={draft.genres.mode}>
								<option value="replace">Replace</option>
								<option value="merge">Merge</option>
								<option value="fill_missing">Fill missing</option>
							</select>
						</label>
						<label class="grid gap-1.5 text-sm">
							<span>Maximum genres</span>
							<input
								type="number"
								min="1"
								max="50"
								class="input input-bordered bg-base-100"
								bind:value={draft.genres.maximum_count}
							/>
						</label>
						<details class="management-editor-advanced sm:col-span-2">
							<summary>
								<span
									><strong>Advanced genre rules</strong><small
										>Thresholds, filtering, casing, and aliases</small
									></span
								>
								<ChevronRight class="h-4 w-4" />
							</summary>
							<div class="mt-3 grid gap-3 sm:grid-cols-2">
								<label class="grid gap-1.5 text-sm">
									<span>MusicBrainz minimum votes</span>
									<input
										type="number"
										min="0"
										class="input input-bordered bg-base-100"
										bind:value={draft.genres.musicbrainz_minimum_count}
									/>
								</label>
								<label class="grid gap-1.5 text-sm">
									<span>ListenBrainz minimum votes</span>
									<input
										type="number"
										min="0"
										class="input input-bordered bg-base-100"
										bind:value={draft.genres.listenbrainz_minimum_count}
									/>
								</label>
								<label class="grid gap-1.5 text-sm">
									<span>Last.fm minimum weight</span>
									<input
										type="number"
										min="0"
										class="input input-bordered bg-base-100"
										bind:value={draft.genres.lastfm_minimum_weight}
									/>
								</label>
								<label class="grid gap-1.5 text-sm">
									<span>Maximum ancestry depth</span>
									<input
										type="number"
										min="0"
										max="20"
										class="input input-bordered bg-base-100"
										bind:value={draft.genres.maximum_ancestry_depth}
									/>
								</label>
								<label class="management-master-toggle"
									><input
										type="checkbox"
										class="toggle toggle-sm"
										bind:checked={draft.genres.listenbrainz_curated_only}
									/><span
										><strong>Curated ListenBrainz tags only</strong><small
											>Reject uncurated community tags.</small
										></span
									></label
								>
								<label class="management-master-toggle"
									><input
										type="checkbox"
										class="toggle toggle-sm"
										bind:checked={draft.genres.lastfm_whitelist_only}
									/><span
										><strong>Allowlisted Last.fm tags only</strong><small
											>Require a configured accepted genre.</small
										></span
									></label
								>
								<label class="management-master-toggle"
									><input
										type="checkbox"
										class="toggle toggle-sm"
										bind:checked={draft.genres.canonicalize}
									/><span
										><strong>Canonicalize genres</strong><small>Apply aliases and ancestry.</small
										></span
									></label
								>
								<label class="management-master-toggle"
									><input
										type="checkbox"
										class="toggle toggle-sm"
										bind:checked={draft.genres.write_primary_only_for_constrained_formats}
									/><span
										><strong>Primary genre on constrained formats</strong><small
											>Use one value where multi-values are lossy.</small
										></span
									></label
								>
								<label class="grid gap-1.5 text-sm"
									><span>Genre allowlist (one per line)</span><textarea
										class="textarea textarea-bordered min-h-24 bg-base-100"
										value={draft.genres.allowlist.join('\n')}
										oninput={(event) => (draft.genres.allowlist = lines(event.currentTarget.value))}
									></textarea></label
								>
								<label class="grid gap-1.5 text-sm"
									><span>Genre denylist (one per line)</span><textarea
										class="textarea textarea-bordered min-h-24 bg-base-100"
										value={draft.genres.denylist.join('\n')}
										oninput={(event) => (draft.genres.denylist = lines(event.currentTarget.value))}
									></textarea></label
								>
								<label class="grid gap-1.5 text-sm sm:col-span-2"
									><span>Preferred casing (one per line)</span><textarea
										class="textarea textarea-bordered min-h-20 bg-base-100"
										value={draft.genres.preferred_casing.join('\n')}
										oninput={(event) =>
											(draft.genres.preferred_casing = lines(event.currentTarget.value))}
									></textarea></label
								>
								<div class="space-y-2 sm:col-span-2">
									<div class="flex items-center justify-between">
										<strong class="text-sm">Genre aliases</strong><button
											class="btn btn-ghost btn-xs"
											onclick={addGenreAlias}><Plus class="h-3.5 w-3.5" /> Add alias</button
										>
									</div>
									{#each draft.genres.aliases as alias, index (`${index}:${alias.source}`)}
										<div class="grid grid-cols-[1fr_auto_1fr_auto] items-center gap-2">
											<input
												class="input input-bordered input-sm bg-base-100"
												bind:value={alias.source}
												aria-label={`Genre alias source ${index + 1}`}
											/>
											<span aria-hidden="true">→</span>
											<input
												class="input input-bordered input-sm bg-base-100"
												bind:value={alias.target}
												aria-label={`Genre alias target ${index + 1}`}
											/>
											<button
												class="btn btn-ghost btn-xs btn-square text-error"
												aria-label={`Remove genre alias ${index + 1}`}
												onclick={() =>
													(draft.genres.aliases = draft.genres.aliases.filter(
														(_, valueIndex) => valueIndex !== index
													))}><Trash2 class="h-3.5 w-3.5" /></button
											>
										</div>
									{/each}
								</div>
							</div>
						</details>
					{/if}
				</div>
			</details>

			<details class="management-editor-section" data-active={artworkEnabled}>
				<summary class="management-editor-summary">
					<span class="management-editor-icon"><Image class="h-4 w-4" /></span>
					<span class="management-editor-summary__copy"
						><strong>Artwork</strong><small>Embedded and external cover decisions</small></span
					>
					<span class="management-editor-overview">
						<span class="management-editor-state" data-active={artworkEnabled}
							>{artworkEnabled ? 'On' : 'Off'}</span
						>
						<span class="management-editor-chip" data-active={draft.artwork.embedded_enabled}
							>{draft.artwork.embedded_enabled ? 'Embedded on' : 'Embedded off'}</span
						>
						<span class="management-editor-chip" data-active={draft.artwork.external_enabled}
							>{draft.artwork.external_enabled ? 'External on' : 'External off'}</span
						>
					</span>
					<ChevronRight class="h-4 w-4 management-editor-chevron" />
				</summary>
				<div class="mt-4 grid gap-3 sm:grid-cols-2">
					<label class="management-master-toggle">
						<input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.artwork.embedded_enabled}
						/>
						<span
							><strong>Embedded artwork</strong><small
								>Write selected images into supported audio containers.</small
							></span
						>
					</label>
					<label class="management-master-toggle">
						<input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.artwork.external_enabled}
						/>
						<span
							><strong>External artwork</strong><small
								>Create named image files beside the album.</small
							></span
						>
					</label>
					<section
						class="sm:col-span-2 rounded-xl border border-base-content/10 bg-base-100/35 p-3"
					>
						<div class="flex items-end justify-between gap-3">
							<div>
								<strong class="text-sm">Artwork source priority</strong>
								<p class="text-xs text-base-content/55">Sources are tried from top to bottom.</p>
							</div>
							<label class="grid gap-1 text-xs">
								<span class="sr-only">Add artwork source</span>
								<select
									class="select select-bordered select-sm bg-base-100"
									value=""
									onchange={(event) => {
										addArtworkProvider(event.currentTarget.value);
										event.currentTarget.value = '';
									}}
								>
									<option value="">Add source…</option>
									{#each artworkProviders.filter((provider) => !draft.artwork.providers.includes(provider.value)) as provider (provider.value)}
										<option value={provider.value}>{provider.label}</option>
									{/each}
								</select>
							</label>
						</div>
						<ol class="mt-3 grid gap-2" aria-label="Artwork source priority">
							{#each draft.artwork.providers as provider, index (provider)}
								<li
									class="flex items-center gap-2 rounded-lg border border-base-content/10 bg-base-100 px-3 py-2"
								>
									<span class="badge badge-ghost badge-sm font-mono">{index + 1}</span>
									<span class="min-w-0 flex-1 text-sm">{artworkProviderLabel(provider)}</span>
									<button
										class="btn btn-ghost btn-xs btn-square"
										aria-label={`Move ${artworkProviderLabel(provider)} earlier`}
										disabled={index === 0}
										onclick={() => moveArtworkProvider(index, -1)}
										><ArrowUp class="h-3.5 w-3.5" /></button
									>
									<button
										class="btn btn-ghost btn-xs btn-square"
										aria-label={`Move ${artworkProviderLabel(provider)} later`}
										disabled={index === draft.artwork.providers.length - 1}
										onclick={() => moveArtworkProvider(index, 1)}
										><ArrowDown class="h-3.5 w-3.5" /></button
									>
									<button
										class="btn btn-ghost btn-xs btn-square text-error"
										aria-label={`Remove ${artworkProviderLabel(provider)}`}
										onclick={() =>
											(draft.artwork.providers = draft.artwork.providers.filter(
												(value) => value !== provider
											))}><X class="h-3.5 w-3.5" /></button
									>
								</li>
							{/each}
						</ol>
					</section>
					<div class="management-choice-grid sm:col-span-2" aria-label="Artwork image types">
						{#each artworkTypes as imageType (imageType.value)}
							<label
								><input
									type="checkbox"
									class="checkbox checkbox-xs"
									checked={draft.artwork.image_types.includes(imageType.value)}
									onchange={(event) =>
										(draft.artwork.image_types = toggled(
											draft.artwork.image_types,
											imageType.value,
											event.currentTarget.checked
										))}
								/><span>{imageType.label}</span></label
							>
						{/each}
					</div>
					<details class="management-editor-advanced sm:col-span-2">
						<summary>
							<span
								><strong>Advanced artwork rules</strong><small
									>Dimensions, formats, replacement, and naming</small
								></span
							>
							<ChevronRight class="h-4 w-4" />
						</summary>
						<div class="mt-3 grid gap-3 sm:grid-cols-2">
							<p class="sm:col-span-2 text-xs text-base-content/55">
								A size of 0 means unlimited. Minimum dimensions filter candidates; maximum sizes
								resize outputs when needed.
							</p>
							<label class="grid gap-1.5 text-sm"
								><span>Minimum width</span><input
									type="number"
									min="0"
									class="input input-bordered bg-base-100"
									bind:value={draft.artwork.minimum_width}
								/></label
							>
							<label class="grid gap-1.5 text-sm"
								><span>Minimum height</span><input
									type="number"
									min="0"
									class="input input-bordered bg-base-100"
									bind:value={draft.artwork.minimum_height}
								/></label
							>
							<label class="grid gap-1.5 text-sm"
								><span>Download size</span><select
									class="select select-bordered bg-base-100"
									bind:value={draft.artwork.download_size}
									><option value="full">Full size</option><option value="1200">1200 px</option
									><option value="500">500 px</option><option value="250">250 px</option></select
								></label
							>
							<label class="management-master-toggle"
								><input
									type="checkbox"
									class="toggle toggle-sm"
									bind:checked={draft.artwork.approved_only}
								/><span
									><strong>Approved cover art only</strong><small
										>Require provider approval where available.</small
									></span
								></label
							>
							{#if draft.artwork.embedded_enabled}
								<label class="grid gap-1.5 text-sm"
									><span>Embedded maximum size</span><input
										type="number"
										min="0"
										class="input input-bordered bg-base-100"
										bind:value={draft.artwork.embedded_maximum_size}
									/></label
								>
								<label class="grid gap-1.5 text-sm"
									><span>Embedded format</span><select
										class="select select-bordered bg-base-100"
										bind:value={draft.artwork.embedded_format}
										><option value="original">Original</option><option value="jpeg">JPEG</option
										><option value="png">PNG</option><option value="webp">WebP</option></select
									></label
								>
								<label class="management-master-toggle"
									><input
										type="checkbox"
										class="toggle toggle-sm"
										bind:checked={draft.artwork.embedded_front_only}
									/><span
										><strong>Embed front art only</strong><small
											>Do not embed additional image types.</small
										></span
									></label
								>
							{/if}
							{#if draft.artwork.external_enabled}
								<label class="grid gap-1.5 text-sm"
									><span>External maximum size</span><input
										type="number"
										min="0"
										class="input input-bordered bg-base-100"
										bind:value={draft.artwork.external_maximum_size}
									/></label
								>
								<label class="grid gap-1.5 text-sm"
									><span>External format</span><select
										class="select select-bordered bg-base-100"
										bind:value={draft.artwork.external_format}
										><option value="original">Original</option><option value="jpeg">JPEG</option
										><option value="png">PNG</option><option value="webp">WebP</option></select
									></label
								>
								<label class="management-master-toggle"
									><input
										type="checkbox"
										class="toggle toggle-sm"
										bind:checked={draft.artwork.external_front_only}
									/><span
										><strong>External front art only</strong><small
											>Do not create other image types.</small
										></span
									></label
								>
								<label class="management-master-toggle"
									><input
										type="checkbox"
										class="toggle toggle-sm"
										bind:checked={draft.artwork.overwrite_external_files}
									/><span
										><strong>Overwrite external artwork</strong><small
											>Leaves existing files untouched and reports naming collisions.</small
										></span
									></label
								>
							{/if}
							<label class="management-master-toggle"
								><input
									type="checkbox"
									class="toggle toggle-sm"
									bind:checked={draft.artwork.never_replace_with_smaller}
								/><span
									><strong>Never replace with smaller</strong><small
										>Compare image dimensions per file and type.</small
									></span
								></label
							>
							<label class="grid gap-1.5 text-sm"
								><span>Local file patterns (one per line)</span><textarea
									class="textarea textarea-bordered min-h-24 bg-base-100"
									value={draft.artwork.local_file_patterns.join('\n')}
									oninput={(event) =>
										(draft.artwork.local_file_patterns = lines(event.currentTarget.value))}
								></textarea></label
							>
							<fieldset class="grid gap-2 text-sm">
								<legend class="mb-1">Preserve existing image types</legend>
								{#each artworkTypes as imageType (imageType.value)}
									<label class="flex items-center gap-2 text-xs"
										><input
											type="checkbox"
											class="checkbox checkbox-xs"
											checked={draft.artwork.preserve_existing_types.includes(imageType.value)}
											onchange={(event) =>
												(draft.artwork.preserve_existing_types = toggled(
													draft.artwork.preserve_existing_types,
													imageType.value,
													event.currentTarget.checked
												))}
										/><span>{imageType.label}</span></label
									>
								{/each}
							</fieldset>
							{#if draft.artwork.external_enabled}
								<label class="grid gap-1.5 text-sm sm:col-span-2"
									><span>External artwork naming script</span><select
										class="select select-bordered bg-base-100"
										bind:value={draft.artwork.external_naming_script_id}
										><option value={null}>Default album filenames (cover.jpg, back.jpg, …)</option
										>{#each scripts as script (script.id)}<option value={script.id}
												>{script.name}</option
											>{/each}</select
									></label
								>
							{/if}
						</div>
					</details>
				</div>
			</details>

			<details class="management-editor-section" data-active={organizationEnabled}>
				<summary class="management-editor-summary">
					<span class="management-editor-icon"><FolderCog class="h-4 w-4" /></span>
					<span class="management-editor-summary__copy"
						><strong>File naming and organization</strong><small
							>Paths, sidecars, and source cleanup</small
						></span
					>
					<span class="management-editor-overview">
						<span class="management-editor-state" data-active={organizationEnabled}
							>{organizationEnabled ? 'On' : 'Off'}</span
						>
						<span class="management-editor-chip" data-active={draft.organization.rename_enabled}
							>{draft.organization.rename_enabled ? 'Rename on' : 'Rename off'}</span
						>
						<span class="management-editor-chip" data-active={draft.organization.move_enabled}
							>{draft.organization.move_enabled ? 'Move on' : 'Move off'}</span
						>
						<span class="management-editor-chip" data-active={draft.organization.move_sidecars}
							>{draft.organization.move_sidecars ? 'Sidecars on' : 'Sidecars off'}</span
						>
					</span>
					<ChevronRight class="h-4 w-4 management-editor-chevron" />
				</summary>
				<div class="mt-4 space-y-4">
					<div class="grid gap-3 sm:grid-cols-3">
						<label class="management-master-toggle"
							><input
								type="checkbox"
								class="toggle toggle-sm"
								bind:checked={draft.organization.rename_enabled}
							/><span><strong>Rename</strong><small>Render the naming script.</small></span></label
						>
						<label class="management-master-toggle"
							><input
								type="checkbox"
								class="toggle toggle-sm"
								bind:checked={draft.organization.move_enabled}
							/><span><strong>Move</strong><small>Organize within the root.</small></span></label
						>
						<label class="management-master-toggle"
							><input
								type="checkbox"
								class="toggle toggle-sm"
								bind:checked={draft.organization.move_sidecars}
							/><span><strong>Sidecars</strong><small>Move matched album files.</small></span
							></label
						>
					</div>
					<p class="text-xs font-semibold uppercase tracking-wider text-base-content/55">
						Scripts used by this profile
					</p>
					<div class="mb-3 grid gap-3 sm:grid-cols-2">
						<label class="grid gap-1 text-xs">
							<span class="font-semibold">Single-disc naming script</span>
							<select
								class="select select-bordered select-sm bg-base-100"
								bind:value={draft.organization.naming_script_id}
							>
								{#each scripts as script (script.id)}<option value={script.id}>{script.name}</option
									>{/each}
							</select>
						</label>
						<label class="grid gap-1 text-xs">
							<span class="font-semibold">Multi-disc naming script</span>
							<select
								class="select select-bordered select-sm bg-base-100"
								bind:value={draft.organization.multi_disc_naming_script_id}
							>
								<option value={null}>Use the single-disc script</option>
								{#each scripts as script (script.id)}<option value={script.id}>{script.name}</option
									>{/each}
							</select>
						</label>
					</div>
					<p class="mb-3 text-xs text-base-content/60">
						Releases with more than one MusicBrainz audio medium use the multi-disc script.
					</p>
					<LibraryManagementScriptEditor
						kind="naming"
						{scripts}
						selectedIds={[
							draft.organization.naming_script_id,
							...(draft.organization.multi_disc_naming_script_id
								? [draft.organization.multi_disc_naming_script_id]
								: [])
						]}
						onchange={updateNamingScripts}
					/>
					<details class="management-editor-advanced">
						<summary>
							<span
								><strong>Path compatibility</strong><small
									>Platform-safe names and path limits</small
								></span
							>
							<ChevronRight class="h-4 w-4" />
						</summary>
						<div class="mt-3 grid gap-3 sm:grid-cols-2">
							<label class="management-master-toggle"
								><input
									type="checkbox"
									class="toggle toggle-sm"
									bind:checked={draft.organization.compatibility.windows_compatible}
								/><span
									><strong>Windows-compatible names</strong><small
										>Apply reserved-character and device-name rules on every host.</small
									></span
								></label
							>
							<label class="management-master-toggle"
								><input
									type="checkbox"
									class="toggle toggle-sm"
									bind:checked={draft.organization.compatibility.windows_legacy_path_limit}
								/><span
									><strong>Legacy Windows path limit</strong><small
										>Keep the absolute path within 259 characters.</small
									></span
								></label
							>
							<label class="management-master-toggle"
								><input
									type="checkbox"
									class="toggle toggle-sm"
									bind:checked={draft.organization.compatibility.replace_non_ascii}
								/><span
									><strong>Replace non-ASCII</strong><small
										>Use compatibility transliteration.</small
									></span
								></label
							>
							<label class="management-master-toggle"
								><input
									type="checkbox"
									class="toggle toggle-sm"
									bind:checked={draft.organization.compatibility.replace_spaces_with_underscores}
								/><span
									><strong>Spaces to underscores</strong><small>Applies after rendering.</small
									></span
								></label
							>
							<label class="grid gap-1.5 text-sm"
								><span>Invalid-character replacement</span><input
									class="input input-bordered bg-base-100"
									maxlength="8"
									bind:value={draft.organization.compatibility.separator_replacement}
								/><small class="text-base-content/55"
									>Replaces path separators and characters unsafe for the selected platform rules.</small
								></label
							>
							<label class="grid gap-1.5 text-sm"
								><span>Unicode normalization</span><select
									class="select select-bordered bg-base-100"
									bind:value={draft.organization.compatibility.unicode_normalization}
									><option value="NFC">NFC</option><option value="NFKC">NFKC</option></select
								><small class="text-base-content/55"
									>NFC preserves character distinctions; NFKC also folds compatibility forms.</small
								></label
							>
							<label class="grid gap-1.5 text-sm"
								><span>Maximum component length</span><input
									type="number"
									min="1"
									class="input input-bordered bg-base-100"
									bind:value={draft.organization.compatibility.maximum_component_length}
								/><small class="text-base-content/55"
									>Longer folder or file names are shortened safely.</small
								></label
							>
							<label class="grid gap-1.5 text-sm"
								><span>Maximum path length</span><input
									type="number"
									min="1"
									class="input input-bordered bg-base-100"
									bind:value={draft.organization.compatibility.maximum_path_length}
								/><small class="text-base-content/55"
									>A dry run blocks paths that still exceed this full-path limit.</small
								></label
							>
							<label class="grid gap-1.5 text-sm sm:col-span-2"
								><span>Extension case</span><select
									class="select select-bordered bg-base-100"
									bind:value={draft.organization.compatibility.extension_case}
									><option value="preserve">Preserve</option><option value="lower">Lowercase</option
									><option value="upper">Uppercase</option></select
								><small class="text-base-content/55"
									>Changes only the filename extension, never the audio format.</small
								></label
							>
						</div>
					</details>
					<details class="management-editor-advanced">
						<summary>
							<span
								><strong>Sidecars and source cleanup</strong><small
									>Matched files and verified post-move cleanup</small
								></span
							>
							<ChevronRight class="h-4 w-4" />
						</summary>
						<div class="mt-3 grid gap-3 sm:grid-cols-2">
							<label class="grid gap-1.5 text-sm sm:col-span-2">
								<span>Sidecar patterns (one per line)</span>
								<textarea
									class="textarea textarea-bordered min-h-24 bg-base-100 font-mono text-xs"
									value={draft.organization.sidecar_patterns.join('\n')}
									oninput={(event) =>
										(draft.organization.sidecar_patterns = lines(event.currentTarget.value))}
								></textarea>
							</label>
							<label class="grid gap-1.5 text-sm"
								><span>Source after confirmed move</span><select
									class="select select-bordered bg-base-100"
									bind:value={draft.organization.source_cleanup}
									><option value="keep">Keep source</option><option
										value="remove_after_confirmed_move">Remove verified source</option
									></select
								><small class="text-base-content/55"
									>Removal happens only after publication and catalog commit are verified.</small
								></label
							>
							<label class="management-master-toggle sm:mt-6"
								><input
									type="checkbox"
									class="toggle toggle-sm"
									bind:checked={draft.organization.remove_empty_directories}
								/><span
									><strong>Remove empty directories</strong><small
										>Only after verified cleanup.</small
									></span
								></label
							>
						</div>
					</details>
				</div>
			</details>

			<details class="management-editor-section" data-active="true">
				<summary class="management-editor-summary">
					<span class="management-editor-icon"><ShieldCheck class="h-4 w-4" /></span>
					<span class="management-editor-summary__copy"
						><strong>Preservation and format safety</strong><small
							>Compatibility, scrub, validation, and notifications</small
						></span
					>
					<span class="management-editor-overview">
						<span class="management-editor-state" data-active="true">Protected</span>
						<span class="management-editor-chip" data-active={enabledSafeguardCount > 0}
							>{enabledSafeguardCount} safeguards</span
						>
						<span class="management-editor-chip"
							>ID3v{draft.metadata.format_compatibility.id3_version}</span
						>
						{#if draft.notification.refresh_external_servers}
							<span class="management-editor-chip" data-active="true">Media refresh on</span>
						{/if}
					</span>
					<ChevronRight class="h-4 w-4 management-editor-chevron" />
				</summary>
				<div class="mt-4 grid gap-3 sm:grid-cols-2">
					<label class="management-master-toggle"
						><input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.metadata.scrub_unmanaged_tags}
						/><span
							><strong>Scrub unmanaged tags</strong><small
								>Explicitly remove tags outside the allowlist.</small
							></span
						></label
					>
					<label class="management-master-toggle"
						><input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.metadata.preserve_embedded_art_during_scrub}
						/><span
							><strong>Preserve embedded art</strong><small
								>Keep pictures during an explicit scrub.</small
							></span
						></label
					>
					<label class="management-master-toggle"
						><input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.file_behavior.preserve_timestamps}
						/><span
							><strong>Preserve timestamps</strong><small>Restore source times after publish.</small
							></span
						></label
					>
					<label class="management-master-toggle"
						><input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.file_behavior.validate_written_metadata}
						/><span
							><strong>Read-back validation</strong><small
								>Reject a staged file that does not match.</small
							></span
						></label
					>
					<details class="management-editor-advanced sm:col-span-2">
						<summary>
							<span
								><strong>Format compatibility</strong><small
									>ID3, constrained formats, and preserved fields</small
								></span
							>
							<ChevronRight class="h-4 w-4" />
						</summary>
						<div class="mt-3 grid gap-3 sm:grid-cols-2">
							<label class="grid gap-1.5 text-sm"
								><span>ID3 version</span><select
									class="select select-bordered bg-base-100"
									value={draft.metadata.format_compatibility.id3_version}
									onchange={(event) => {
										draft.metadata.format_compatibility.id3_version = event.currentTarget.value as
											'2.4' | '2.3';
										if (event.currentTarget.value === '2.3')
											draft.metadata.format_compatibility.id3_text_encoding = 'utf16';
									}}
									><option value="2.4">ID3v2.4</option><option value="2.3">ID3v2.3</option></select
								></label
							>
							{#if draft.metadata.format_compatibility.id3_version === '2.3'}<label
									class="grid gap-1.5 text-sm"
									><span>ID3v2.3 list delimiter</span><input
										class="input input-bordered bg-base-100"
										bind:value={draft.metadata.format_compatibility.id3v23_join_delimiter}
									/></label
								>{/if}
							<label class="grid gap-1.5 text-sm"
								><span>ID3 text encoding</span><select
									class="select select-bordered bg-base-100"
									bind:value={draft.metadata.format_compatibility.id3_text_encoding}
									>{#if draft.metadata.format_compatibility.id3_version === '2.4'}<option
											value="utf8">UTF-8</option
										>{/if}<option value="utf16">UTF-16</option></select
								></label
							>
							<label class="grid gap-1.5 text-sm"
								><span>MP3 APEv2 tags</span><select
									class="select select-bordered bg-base-100"
									bind:value={draft.metadata.format_compatibility.mp3_apev2_policy}
									><option value="preserve">Preserve</option><option value="remove">Remove</option
									></select
								><small class="text-warning"
									>Remove deletes the complete MP3 APEv2 tag container.</small
								></label
							>
							<label class="grid gap-1.5 text-sm"
								><span>Raw AAC tags</span><select
									class="select select-bordered bg-base-100"
									bind:value={draft.metadata.format_compatibility.raw_aac_tag_policy}
									><option value="save_apev2">Save APEv2</option><option value="do_not_write"
										>Do not write</option
									><option value="remove_apev2">Remove APEv2</option></select
								><small class="text-warning"
									>Do not write preserves raw AAC bytes; Remove APEv2 also removes artwork stored
									there.</small
								></label
							>
							<label class="grid gap-1.5 text-sm"
								><span>WAV tags</span><select
									class="select select-bordered bg-base-100"
									bind:value={draft.metadata.format_compatibility.wav_tag_policy}
									><option value="id3">ID3</option><option value="riff_info">RIFF INFO</option
									><option value="preserve_existing">Preserve existing format</option></select
								><small class="text-warning"
									>Choosing ID3 or RIFF INFO may convert the active WAV tag representation.</small
								></label
							>
							<label class="management-master-toggle"
								><input
									type="checkbox"
									class="toggle toggle-sm"
									bind:checked={draft.metadata.format_compatibility.remove_id3_from_flac}
								/><span
									><strong>Remove stray ID3 from FLAC</strong><small
										>Explicit compatibility cleanup.</small
									></span
								></label
							>
							<label class="grid gap-1.5 text-sm sm:col-span-2"
								><span>Always preserve fields (one per line)</span><textarea
									class="textarea textarea-bordered min-h-20 bg-base-100 font-mono text-xs"
									value={draft.metadata.preserve_fields.join('\n')}
									oninput={(event) =>
										(draft.metadata.preserve_fields = lines(event.currentTarget.value))}
								></textarea></label
							>
						</div>
					</details>
					<label class="management-master-toggle"
						><input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.file_behavior.preserve_permissions}
						/><span
							><strong>Preserve permissions</strong><small
								>Copy source file mode to the published file.</small
							></span
						></label
					>
					<label class="management-master-toggle"
						><input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.file_behavior.strict_capability_gate}
						/><span
							><strong>Strict format capability gate</strong><small
								>Block rather than accept documented representation loss.</small
							></span
						></label
					>
					<label class="management-master-toggle"
						><input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.file_behavior.reject_symlinks}
						/><span
							><strong>Reject symlinks</strong><small
								>Never follow linked audio or sidecar paths.</small
							></span
						></label
					>
					<label class="management-master-toggle"
						><input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.file_behavior.validate_technical_audio}
						/><span
							><strong>Validate technical audio</strong><small
								>Verify codec and stream properties after staging.</small
							></span
						></label
					>
					<label class="management-master-toggle">
						<input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.identity.automatic_edition_acceptance_enabled}
						/><span
							><strong>Automatic exact-edition acceptance</strong><small
								>During identity checks, accept the best proven MusicBrainz edition automatically
								(score 0.95+, clear winner). Every acceptance is audited and can be undone.</small
							></span
						></label
					>
					<details class="management-editor-advanced sm:col-span-2">
						<summary>
							<span
								><strong>Post-write notifications</strong><small
									>Catalog and external media server refresh</small
								></span
							>
							<ChevronRight class="h-4 w-4" />
						</summary>
						<div class="mt-3 grid gap-3 sm:grid-cols-2">
							<div class="management-master-toggle">
								<Check class="h-4 w-4 shrink-0 text-success" />
								<span
									><strong>DroppedNeedle catalog updates immediately</strong><small
										>Committed tags and paths are updated as part of every successful operation.</small
									></span
								>
							</div>
							<label class="management-master-toggle"
								><input
									type="checkbox"
									class="toggle toggle-sm"
									bind:checked={draft.notification.refresh_external_servers}
								/><span
									><strong>Refresh media servers</strong><small
										>Notify enabled servers after commit.</small
									></span
								></label
							>
						</div>
					</details>
				</div>
			</details>

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
				<button
					class="btn btn-ghost"
					onclick={(event) => requestClose(event.currentTarget)}
					disabled={saving}>Cancel</button
				>
				<button class="btn management-btn" onclick={() => void save()} disabled={saving || !dirty}>
					{#if saving}<span class="loading loading-spinner loading-sm"></span>{/if}
					Save profile
				</button>
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
			<button class="btn btn-ghost" onclick={() => resetDialog.close()}>Cancel</button>
			<button class="btn management-btn" onclick={resetSection}>Reset section</button>
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
			<button class="btn btn-ghost" onclick={() => discardDialog.close()}>Keep editing</button>
			<button class="btn btn-error" onclick={discardChanges}>Discard changes</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close discard confirmation">close</button>
	</form>
</dialog>
