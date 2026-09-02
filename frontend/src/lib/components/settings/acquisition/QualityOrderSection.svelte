<script lang="ts">
	import {
		ChevronDown,
		ChevronRight,
		ChevronUp,
		Disc3,
		GripVertical,
		Music2,
		Pencil,
		Trash2,
		X
	} from 'lucide-svelte';

	import type { QualityRecipeEntry, QualityRecipeQuality } from '$lib/types';

	import {
		FLAC_OPTIONS,
		MP3_OPTIONS,
		PRESETS,
		PRESET_KEYS,
		moveRecipeEntry,
		presetMatches,
		recipeEntryLabel,
		recipeEntrySummary,
		recipeFingerprint,
		recipeSummary,
		recipeWithIds,
		standardEntryDuplicate,
		standardRecipeEntry,
		UNKNOWN_BEHAVIOR_OPTIONS,
		unknownQualitySummary,
		validateRecipeEntry,
		type PresetKey,
		type RecipeDraftEntry,
		type RecipeMigrationStatus
	} from './qualityRecipeModel';

	type FlacQuality = 'cd' | '24_48' | '24_96' | '24_192' | 'hi_res' | 'custom';
	type Mp3Quality = 'below_192' | '192_255' | '256_319' | '320_plus' | 'custom';

	const FORMAT_ICONS = { flac: Disc3, mp3: Music2 } as const;
	interface Props {
		qualityRecipe: RecipeDraftEntry[];
		unknownQualityBehavior: string;
		baseline?: string | null;
		baselineRecipe?: readonly QualityRecipeEntry[];
		baselineUnknownQualityBehavior?: string;
		migrationStatus?: RecipeMigrationStatus | null;
		migrationMessage?: string | null;
		saving?: boolean;
		ondiscard?: () => void;
	}

	let {
		qualityRecipe = $bindable(),
		unknownQualityBehavior = $bindable(),
		baseline = null,
		baselineRecipe = [],
		baselineUnknownQualityBehavior = 'allow_as_fallback',
		migrationStatus = null,
		migrationMessage = null,
		saving = false,
		ondiscard
	}: Props = $props();

	let flacQuality = $state<FlacQuality>('cd');
	let mp3Quality = $state<Mp3Quality>('320_plus');
	let customFlacBitDepth = $state('');
	let customFlacSampleRate = $state('');
	let customMp3Min = $state('');
	let customMp3Target = $state('');
	let customMp3Max = $state('');
	let editingId = $state<string | null>(null);
	let editingFormat = $state<'flac' | 'mp3' | null>(null);
	let pendingPresetKey = $state<PresetKey | null>(null);
	let presetTrigger = $state<HTMLElement | null>(null);
	let confirmDialog = $state<HTMLDialogElement | null>(null);
	let dragId = $state<string | null>(null);
	let focusId = $state<string | null>(null);
	let recipeSequence = $state(0);

	const currentFingerprint = $derived(recipeFingerprint(qualityRecipe, unknownQualityBehavior));
	const dirty = $derived(baseline !== null && currentFingerprint !== baseline);
	const selectedPreset = $derived(
		PRESET_KEYS.find((key) => presetMatches(qualityRecipe, unknownQualityBehavior, PRESETS[key]))
	);
	const savedPreset = $derived.by(() => {
		if (baseline === null) return undefined;
		return PRESET_KEYS.find(
			(key) =>
				recipeFingerprint(baselineRecipe, baselineUnknownQualityBehavior) ===
				recipeFingerprint(PRESETS[key].recipe, PRESETS[key].unknown_quality_behavior)
		);
	});
	const pendingPresetLabel = $derived(pendingPresetKey ? PRESETS[pendingPresetKey].label : '');
	const summary = $derived(recipeSummary(qualityRecipe));
	const liveSummary = $derived(`${summary} ${unknownQualitySummary(unknownQualityBehavior)}`);
	const migrationAlert = $derived(
		migrationStatus === 'v1' ||
			migrationStatus === 'invalid' ||
			migrationStatus === 'non_convertible' ||
			migrationStatus === 'projected'
	);
	const recipeValidationError = $derived.by(() => {
		for (let index = 0; index < qualityRecipe.length; index += 1) {
			const error = validateRecipeEntry(
				qualityRecipe[index],
				qualityRecipe.slice(0, index),
				qualityRecipe[index].id
			);
			if (error) return `Position ${index + 1}: ${error}`;
		}
		return null;
	});
	const flacCandidate = $derived.by((): QualityRecipeEntry => {
		if (flacQuality === 'custom') {
			return {
				format: 'flac',
				quality: 'custom',
				bit_depth: Number(customFlacBitDepth),
				sample_rate_hz: Number(customFlacSampleRate)
			};
		}
		return standardRecipeEntry('flac', flacQuality);
	});
	const mp3Candidate = $derived.by((): QualityRecipeEntry => {
		if (mp3Quality === 'custom') {
			return {
				format: 'mp3',
				quality: 'custom',
				min_bitrate_kbps: Number(customMp3Min),
				target_bitrate_kbps: Number(customMp3Target),
				max_bitrate_kbps: Number(customMp3Max)
			};
		}
		return standardRecipeEntry('mp3', mp3Quality);
	});
	const flacFormError = $derived(
		validateRecipeEntry(
			flacCandidate,
			qualityRecipe,
			editingFormat === 'flac' ? (editingId ?? undefined) : undefined
		)
	);
	const mp3FormError = $derived(
		validateRecipeEntry(
			mp3Candidate,
			qualityRecipe,
			editingFormat === 'mp3' ? (editingId ?? undefined) : undefined
		)
	);

	$effect(() => {
		const id = focusId;
		if (!id) return;
		queueMicrotask(() => {
			document.querySelector<HTMLButtonElement>(`[data-recipe-handle="${id}"]`)?.focus();
			focusId = null;
		});
	});

	function requestPreset(key: PresetKey, event: MouseEvent) {
		if (!dirty && migrationStatus !== 'non_convertible' && migrationStatus !== 'invalid') {
			applyPreset(key);
			return;
		}
		presetTrigger = event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
		pendingPresetKey = key;
		confirmDialog?.showModal();
	}

	function applyPreset(key: PresetKey) {
		const preset = PRESETS[key];
		qualityRecipe = recipeWithIds(preset.recipe);
		unknownQualityBehavior = preset.unknown_quality_behavior;
		editingId = null;
		editingFormat = null;
		resetForm();
	}

	function confirmPreset() {
		if (pendingPresetKey) applyPreset(pendingPresetKey);
		confirmDialog?.close();
	}

	function onDialogClose() {
		pendingPresetKey = null;
		presetTrigger?.focus();
		presetTrigger = null;
	}

	function resetForm() {
		flacQuality = 'cd';
		mp3Quality = '320_plus';
		customFlacBitDepth = '';
		customFlacSampleRate = '';
		customMp3Min = '';
		customMp3Target = '';
		customMp3Max = '';
	}

	function beginEdit(entry: RecipeDraftEntry) {
		editingId = entry.id;
		editingFormat = entry.format;
		if (entry.format === 'flac') {
			flacQuality = entry.quality as FlacQuality;
			if (entry.quality === 'custom') {
				customFlacBitDepth = String(entry.bit_depth ?? '');
				customFlacSampleRate = String(entry.sample_rate_hz ?? '');
			}
		} else {
			mp3Quality = entry.quality as Mp3Quality;
			if (entry.quality === 'custom') {
				customMp3Min = String(entry.min_bitrate_kbps ?? '');
				customMp3Target = String(entry.target_bitrate_kbps ?? '');
				customMp3Max = String(entry.max_bitrate_kbps ?? '');
			}
		}
		focusId = entry.id;
	}

	function cancelEdit() {
		editingId = null;
		editingFormat = null;
		resetForm();
	}

	function allocateId() {
		let candidate = '';
		do {
			recipeSequence += 1;
			candidate = `recipe-${recipeSequence}`;
		} while (qualityRecipe.some((entry) => entry.id === candidate));
		return candidate;
	}

	function saveEntry(format: 'flac' | 'mp3') {
		const candidate = format === 'flac' ? flacCandidate : mp3Candidate;
		const error = format === 'flac' ? flacFormError : mp3FormError;
		const shouldUpdate = editingId !== null && editingFormat === format;
		if (error || saving) return;
		if (shouldUpdate) {
			const id = editingId;
			qualityRecipe = qualityRecipe.map((entry) =>
				entry.id === id ? { ...candidate, id } : entry
			);
			editingId = null;
			editingFormat = null;
			resetForm();
			focusId = id;
			return;
		}
		const id = allocateId();
		qualityRecipe = [...qualityRecipe, { ...candidate, id }];
		resetForm();
		focusId = id;
	}

	function move(index: number, delta: -1 | 1) {
		const target = index + delta;
		if (target < 0 || target >= qualityRecipe.length || saving) return;
		const id = qualityRecipe[index].id;
		qualityRecipe = moveRecipeEntry(qualityRecipe, index, target);
		focusId = id;
	}

	function onDrop(targetIndex: number) {
		if (!dragId || saving) return;
		const sourceIndex = qualityRecipe.findIndex((entry) => entry.id === dragId);
		if (sourceIndex < 0 || sourceIndex === targetIndex) {
			dragId = null;
			return;
		}
		qualityRecipe = moveRecipeEntry(qualityRecipe, sourceIndex, targetIndex);
		focusId = dragId;
		dragId = null;
	}

	function remove(index: number) {
		if (qualityRecipe.length <= 1 || saving) return;
		const removedId = qualityRecipe[index]?.id;
		const next = qualityRecipe.filter((_, candidateIndex) => candidateIndex !== index);
		qualityRecipe = next;
		if (editingId === removedId) {
			editingId = null;
			editingFormat = null;
		}
		const focusEntry = next[Math.min(index, next.length - 1)];
		if (focusEntry) focusId = focusEntry.id;
	}

	function discard() {
		if (ondiscard) ondiscard();
		else {
			qualityRecipe = recipeWithIds(baselineRecipe);
			unknownQualityBehavior = baselineUnknownQualityBehavior;
		}
		editingId = null;
		editingFormat = null;
		resetForm();
	}
	function duplicatePosition(
		format: QualityRecipeEntry['format'],
		quality: QualityRecipeQuality
	): number | null {
		const excludeId = editingFormat === format ? (editingId ?? undefined) : undefined;
		const duplicate = standardEntryDuplicate({ format, quality }, qualityRecipe, excludeId);
		if (!duplicate) return null;
		return qualityRecipe.indexOf(duplicate) + 1;
	}

	function addHelpText(format: 'flac' | 'mp3', error: string | null): string {
		if (error) return error;
		return format === 'flac'
			? 'Adds this FLAC detail at the end.'
			: 'Adds this MP3 region at the end.';
	}
</script>

<div class="flex min-w-0 flex-col gap-5" data-motion="quality-recipe">
	<p class="sr-only" role="status" aria-live="polite">{liveSummary}</p>

	{#if migrationAlert}
		<div class="alert alert-warning items-start" role="alert">
			<div class="min-w-0">
				<p class="font-medium">Review your saved quality policy</p>
				<p class="text-sm">
					{migrationMessage ||
						'Your saved policy is unchanged until you choose a replacement preset.'}
				</p>
				{#if migrationStatus === 'projected'}
					<p class="mt-1 text-xs text-base-content/70">
						This projected recipe is safe to review. Saving it adopts the v2 recipe for new
						acquisitions.
					</p>
				{:else}
					<p class="mt-1 text-xs text-base-content/70">
						Choose a replacement preset below to create a valid FLAC and MP3 recipe. Active tasks
						keep their saved policy.
					</p>
				{/if}
			</div>
		</div>
	{/if}

	<fieldset class="fieldset min-w-0 border-t border-base-300 pt-4">
		<legend class="fieldset-legend">Presets</legend>
		<div class="grid gap-2 sm:grid-cols-2" role="group" aria-label="Quality presets">
			{#each PRESET_KEYS as key (key)}
				{@const preset = PRESETS[key]}
				<button
					type="button"
					class="btn btn-ghost h-auto min-h-16 items-start justify-between gap-3 border border-base-300 px-3 py-2 text-start motion-reduce:transition-none hover:border-primary/50"
					data-preset={key}
					aria-label={`Apply ${preset.label} preset`}
					onclick={(event) => requestPreset(key, event)}
				>
					<span class="min-w-0">
						<span class="block font-medium">{preset.label}</span>
						<span class="mt-0.5 block text-xs font-normal leading-4 text-base-content/60"
							>{preset.descriptor}</span
						>
					</span>
					<span class="flex shrink-0 flex-col items-end gap-1">
						{#if savedPreset === key}
							<span class="badge badge-primary badge-sm">Current</span>
						{:else if selectedPreset === key}
							<span class="badge badge-outline badge-sm">Selected</span>
						{/if}
					</span>
				</button>
			{/each}
		</div>
	</fieldset>

	<section class="min-w-0 border-y border-base-300 py-4" aria-label="Quality recipe summary">
		<div class="flex flex-wrap items-center justify-between gap-2">
			<p class="text-xs font-semibold uppercase tracking-[0.12em] text-base-content/55">
				Live recipe
			</p>
			{#if dirty}
				<span class="badge badge-warning badge-sm">Unsaved changes</span>
			{/if}
		</div>
		<p class="sr-only">{summary}</p>
		{#if qualityRecipe.length}
			<div class="mt-2 flex flex-wrap items-center gap-1.5" aria-hidden="true">
				{#each qualityRecipe as entry, index (entry.id)}
					{#if index > 0}
						<ChevronRight class="size-3.5 shrink-0 text-base-content/40" aria-hidden="true" />
					{/if}
					<span
						class="rounded-full border border-base-300 bg-base-200/60 px-2.5 py-1 text-xs whitespace-nowrap"
					>
						{recipeEntrySummary(entry)}
					</span>
				{/each}
			</div>
		{:else}
			<p class="mt-2 text-xs text-base-content/55">No recipe entries yet.</p>
		{/if}
		<p class="mt-2 text-xs leading-5 text-base-content/55">
			{unknownQualitySummary(unknownQualityBehavior)} This applies when a complete album cannot map to
			exactly one recipe entry; it is not another preference position.
		</p>
	</section>

	<section aria-label="Ordered quality recipe">
		<div class="mb-3 flex flex-wrap items-end justify-between gap-2">
			<div>
				<h4 class="font-semibold">Try in this order</h4>
				<p class="text-sm text-base-content/60">
					The first match wins. Reordering changes preference only, never entry contents.
				</p>
			</div>
			<span class="text-xs uppercase tracking-[0.12em] text-base-content/45"
				>First to last resort</span
			>
		</div>

		{#if qualityRecipe.length}
			<ol class="space-y-3" aria-label="Ordered quality recipe entries">
				{#each qualityRecipe as entry, index (entry.id)}
					{@const Icon = FORMAT_ICONS[entry.format]}
					<li
						class="quality-recipe-row min-w-0 rounded-box border border-base-300 bg-base-100 p-3 transition-[transform,opacity] duration-150 ease-out motion-reduce:transition-none"
						data-recipe-id={entry.id}
						role="listitem"
						ondragover={(event) => event.preventDefault()}
						ondrop={() => onDrop(index)}
					>
						<div class="flex min-w-0 flex-wrap items-start gap-3">
							<span
								class="grid size-7 shrink-0 self-center place-items-center rounded-full bg-primary font-mono text-xs font-semibold text-primary-content"
								aria-hidden="true"
							>
								{index + 1}
							</span>
							<button
								type="button"
								class="btn btn-ghost btn-square min-h-11 min-w-11 cursor-grab text-base-content/45 motion-reduce:transition-none hover:text-base-content active:cursor-grabbing"
								aria-label={`Drag to reorder ${recipeEntrySummary(entry)}`}
								data-recipe-handle={entry.id}
								draggable="true"
								ondragstart={() => (dragId = entry.id)}
								ondragend={() => (dragId = null)}
								onkeydown={(event) => {
									if (event.key === 'ArrowUp') {
										event.preventDefault();
										move(index, -1);
									} else if (event.key === 'ArrowDown') {
										event.preventDefault();
										move(index, 1);
									}
								}}
							>
								<GripVertical class="size-5" aria-hidden="true" />
							</button>
							<span
								class="mt-1 grid size-9 shrink-0 place-items-center rounded-lg bg-base-200 text-primary"
								aria-hidden="true"
							>
								<Icon class="size-5" />
							</span>
							<div class="min-w-0 flex-1">
								<div class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
									<span
										class="text-xs font-semibold uppercase tracking-[0.12em] text-base-content/50"
										>{entry.format}</span
									>
									<span class="break-words font-medium">{recipeEntryLabel(entry)}</span>
									{#if index === 0}<span class="badge badge-primary badge-sm">First choice</span
										>{/if}
									{#if index === qualityRecipe.length - 1}<span class="badge badge-ghost badge-sm"
											>Last resort</span
										>{/if}
								</div>
								{#if entry.format === 'mp3' && entry.quality === 'custom'}
									<p class="mt-1 text-xs text-base-content/55">
										Inclusive range; target {entry.target_bitrate_kbps} kbps
									</p>
								{:else if entry.format === 'flac' && entry.quality === 'custom'}
									<p class="mt-1 text-xs text-base-content/55">Exact detail match</p>
								{/if}
							</div>
							<div class="flex shrink-0 flex-wrap items-center gap-1">
								<button
									type="button"
									class="btn btn-ghost btn-sm min-h-11 min-w-11 motion-reduce:transition-none"
									aria-label={`Move ${recipeEntrySummary(entry)} up`}
									disabled={saving || index === 0}
									onclick={() => move(index, -1)}
								>
									<ChevronUp class="size-5" aria-hidden="true" />
								</button>
								<button
									type="button"
									class="btn btn-ghost btn-sm min-h-11 min-w-11 motion-reduce:transition-none"
									aria-label={`Move ${recipeEntrySummary(entry)} down`}
									disabled={saving || index === qualityRecipe.length - 1}
									onclick={() => move(index, 1)}
								>
									<ChevronDown class="size-5" aria-hidden="true" />
								</button>
								<button
									type="button"
									class="btn btn-ghost btn-sm min-h-11 gap-1 motion-reduce:transition-none"
									aria-label={`Edit ${recipeEntrySummary(entry)}`}
									disabled={saving}
									onclick={() => beginEdit(entry)}
								>
									<Pencil class="size-4" aria-hidden="true" />
									<span class="hidden sm:inline">Edit</span>
								</button>
								<button
									type="button"
									class="btn btn-ghost btn-sm min-h-11 min-w-11 text-error motion-reduce:transition-none"
									aria-label={`Remove ${recipeEntrySummary(entry)}`}
									title={qualityRecipe.length === 1
										? 'At least one recipe entry must remain.'
										: undefined}
									disabled={saving || qualityRecipe.length === 1}
									onclick={() => remove(index)}
								>
									<Trash2 class="size-4" aria-hidden="true" />
								</button>
							</div>
						</div>
					</li>
				{/each}
			</ol>
		{:else}
			<div
				class="rounded-box border border-dashed border-warning/60 bg-warning/5 p-4 text-sm text-base-content/70"
			>
				No valid entries yet. Choose a preset or add at least one format below.
			</div>
		{/if}
	</section>

	<div class="grid min-w-0 gap-4 lg:grid-cols-2">
		<fieldset class="fieldset flex h-full min-w-0 flex-col border-t border-base-300 pt-4">
			<legend class="fieldset-legend">Add FLAC</legend>
			<div class="grid min-w-0 gap-2 sm:grid-cols-2">
				{#each FLAC_OPTIONS as option (option.quality)}
					<label
						class="flex min-w-0 cursor-pointer items-start gap-2 rounded-lg border border-base-300 px-3 py-2 min-h-11 transition-colors motion-reduce:transition-none hover:border-primary/50"
					>
						<input
							type="radio"
							class="radio radio-primary radio-sm mt-0.5"
							name="flac-quality"
							value={option.quality}
							checked={flacQuality === option.quality}
							onchange={() => (flacQuality = option.quality as FlacQuality)}
						/>
						<span class="min-w-0">
							<span class="block text-sm font-medium">{option.label}</span>
							<span class="block text-xs leading-4 text-base-content/55">{option.detail}</span>
						</span>
					</label>
				{/each}
				<label
					class="flex min-w-0 cursor-pointer items-start gap-2 rounded-lg border border-base-300 px-3 py-2 min-h-11 transition-colors motion-reduce:transition-none hover:border-primary/50 sm:col-span-2"
				>
					<input
						type="radio"
						class="radio radio-primary radio-sm mt-0.5"
						name="flac-quality"
						value="custom"
						checked={flacQuality === 'custom'}
						onchange={() => (flacQuality = 'custom')}
					/>
					<span class="min-w-0"
						><span class="block text-sm font-medium">Custom</span><span
							class="block text-xs leading-4 text-base-content/55"
							>Exact bit depth and sample rate</span
						></span
					>
				</label>
			</div>
			{#if flacQuality === 'custom'}
				<div class="grid min-w-0 gap-2 sm:grid-cols-2">
					<label class="flex min-w-0 flex-col gap-1">
						<span class="text-xs text-base-content/70">Bit depth</span>
						<input
							class="input input-sm w-full"
							type="number"
							min="1"
							max="64"
							step="1"
							placeholder="1-64"
							aria-label="Custom FLAC bit depth"
							bind:value={customFlacBitDepth}
						/>
					</label>
					<label class="flex min-w-0 flex-col gap-1">
						<span class="text-xs text-base-content/70">Sample rate (Hz)</span>
						<input
							class="input input-sm w-full"
							type="number"
							min="8000"
							max="768000"
							step="1"
							placeholder="8000-768000"
							aria-label="Custom FLAC sample rate"
							bind:value={customFlacSampleRate}
						/>
					</label>
				</div>
			{/if}
			<div class="mt-auto flex flex-wrap items-center justify-between gap-2 pt-2">
				<p
					class="text-xs leading-4 {flacFormError ? 'text-error' : 'text-base-content/55'}"
					role={flacFormError ? 'alert' : undefined}
				>
					{addHelpText('flac', flacFormError)}
				</p>
				<button
					type="button"
					class="btn btn-primary btn-sm min-h-11 motion-reduce:transition-none"
					disabled={saving || flacFormError !== null}
					aria-label={editingFormat === 'flac'
						? 'Update FLAC recipe entry'
						: 'Add FLAC recipe entry'}
					onclick={() => saveEntry('flac')}
					>{editingFormat === 'flac' ? 'Update FLAC' : 'Add FLAC'}</button
				>
			</div>
		</fieldset>

		<fieldset class="fieldset flex h-full min-w-0 flex-col border-t border-base-300 pt-4">
			<legend class="fieldset-legend">Add MP3</legend>
			<div class="grid min-w-0 gap-2 sm:grid-cols-2">
				{#each MP3_OPTIONS as option (option.quality)}
					{@const position = duplicatePosition('mp3', option.quality)}
					<label
						class="flex min-w-0 cursor-pointer items-start gap-2 rounded-lg border border-base-300 px-3 py-2 min-h-11 transition-colors motion-reduce:transition-none hover:border-primary/50"
					>
						<input
							type="radio"
							class="radio radio-primary radio-sm mt-0.5"
							name="mp3-quality"
							value={option.quality}
							checked={mp3Quality === option.quality}
							onchange={() => (mp3Quality = option.quality as Mp3Quality)}
						/>
						<span class="min-w-0"
							><span class="block text-sm font-medium">{option.label} kbps</span><span
								class="block text-xs leading-4 text-base-content/55"
								>{position ? `Already at position ${position}` : option.detail}</span
							></span
						>
					</label>
				{/each}
				<label
					class="flex min-w-0 cursor-pointer items-start gap-2 rounded-lg border border-base-300 px-3 py-2 min-h-11 transition-colors motion-reduce:transition-none hover:border-primary/50 sm:col-span-2"
				>
					<input
						type="radio"
						class="radio radio-primary radio-sm mt-0.5"
						name="mp3-quality"
						value="custom"
						checked={mp3Quality === 'custom'}
						onchange={() => (mp3Quality = 'custom')}
					/>
					<span class="min-w-0"
						><span class="block text-sm font-medium">Custom</span><span
							class="block text-xs leading-4 text-base-content/55"
							>Inclusive minimum, target, and maximum</span
						></span
					>
				</label>
			</div>
			{#if mp3Quality === 'custom'}
				<div class="grid min-w-0 gap-2 sm:grid-cols-3">
					<label class="flex min-w-0 flex-col gap-1"
						><span class="text-xs text-base-content/70">Minimum (kbps)</span><input
							class="input input-sm w-full"
							type="number"
							min="16"
							max="2048"
							step="1"
							placeholder="16-2048"
							aria-label="Custom MP3 minimum bitrate"
							bind:value={customMp3Min}
						/></label
					>
					<label class="flex min-w-0 flex-col gap-1"
						><span class="text-xs text-base-content/70">Target (kbps)</span><input
							class="input input-sm w-full"
							type="number"
							min="16"
							max="2048"
							step="1"
							placeholder="16-2048"
							aria-label="Custom MP3 target bitrate"
							bind:value={customMp3Target}
						/></label
					>
					<label class="flex min-w-0 flex-col gap-1"
						><span class="text-xs text-base-content/70">Maximum (kbps)</span><input
							class="input input-sm w-full"
							type="number"
							min="16"
							max="2048"
							step="1"
							placeholder="16-2048"
							aria-label="Custom MP3 maximum bitrate"
							bind:value={customMp3Max}
						/></label
					>
				</div>
			{/if}
			<div class="mt-auto flex flex-wrap items-center justify-between gap-2 pt-2">
				<p
					class="text-xs leading-4 {mp3FormError ? 'text-error' : 'text-base-content/55'}"
					role={mp3FormError ? 'alert' : undefined}
				>
					{addHelpText('mp3', mp3FormError)}
				</p>
				<button
					type="button"
					class="btn btn-primary btn-sm min-h-11 motion-reduce:transition-none"
					disabled={saving || mp3FormError !== null}
					aria-label={editingFormat === 'mp3' ? 'Update MP3 recipe entry' : 'Add MP3 recipe entry'}
					onclick={() => saveEntry('mp3')}
					>{editingFormat === 'mp3' ? 'Update MP3' : 'Add MP3'}</button
				>
			</div>
		</fieldset>
	</div>

	<fieldset class="fieldset min-w-0 border-t border-base-300 pt-4">
		<legend class="fieldset-legend">Unknown or incomplete quality</legend>
		<div class="grid gap-2 lg:grid-cols-3">
			{#each UNKNOWN_BEHAVIOR_OPTIONS as option (option.value)}
				<label
					class="flex min-w-0 cursor-pointer items-start gap-2 rounded-lg border border-base-300 px-3 py-2 min-h-11 transition-colors motion-reduce:transition-none hover:border-primary/50"
				>
					<input
						type="radio"
						class="radio radio-primary radio-sm mt-0.5"
						name="unknown-quality-behavior"
						value={option.value}
						checked={unknownQualityBehavior === option.value}
						onchange={() => (unknownQualityBehavior = option.value)}
					/>
					<span class="min-w-0"
						><span class="block text-sm font-medium">{option.label}</span><span
							class="block text-xs leading-4 text-base-content/55">{option.detail}</span
						></span
					>
				</label>
			{/each}
		</div>
	</fieldset>

	{#if editingId}
		<div
			class="flex flex-wrap items-center justify-between gap-2 rounded-box border border-primary/30 bg-primary/5 p-3"
		>
			<p class="text-sm text-base-content/75">Editing keeps this entry in its current position.</p>
			<button
				type="button"
				class="btn btn-ghost btn-sm min-h-11 gap-1 motion-reduce:transition-none"
				onclick={cancelEdit}><X class="size-4" aria-hidden="true" />Cancel edit</button
			>
		</div>
	{/if}

	<div
		class="flex flex-col gap-2 border-t border-base-300 pt-4 sm:flex-row sm:items-center sm:justify-end"
	>
		{#if dirty}
			<button
				type="button"
				class="btn btn-ghost btn-sm min-h-11 motion-reduce:transition-none"
				disabled={saving}
				onclick={discard}>Discard</button
			>
		{/if}
	</div>
	{#if recipeValidationError}
		<p class="text-xs text-error" role="alert">{recipeValidationError}</p>
	{/if}

	<dialog
		class="modal"
		bind:this={confirmDialog}
		onclose={onDialogClose}
		aria-labelledby="quality-recipe-dialog-title"
		aria-describedby="quality-recipe-dialog-description"
	>
		<div class="modal-box">
			<h3 id="quality-recipe-dialog-title" class="font-semibold">Replace this quality recipe?</h3>
			<p id="quality-recipe-dialog-description" class="mt-2 text-sm text-base-content/70">
				You have unsaved recipe changes. Applying <strong>{pendingPresetLabel}</strong> replaces the current
				order and quality entries.
			</p>
			<div class="modal-action">
				<form method="dialog">
					<button class="btn btn-ghost min-h-11 motion-reduce:transition-none">Cancel</button>
				</form>
				<button
					type="button"
					class="btn btn-primary min-h-11 motion-reduce:transition-none"
					onclick={confirmPreset}>Apply preset</button
				>
			</div>
		</div>
		<form method="dialog" class="modal-backdrop"><button>close</button></form>
	</dialog>
</div>
