<script lang="ts">
	import { goto } from '$app/navigation';
	import { Check, ChevronDown, ListPlus, RotateCcw, ShieldAlert, Tags, X } from 'lucide-svelte';

	import { createLibraryManagementTagEditPreviewMutation } from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import { rememberLibraryManagementPreviewToken } from '$lib/queries/library-management/LibraryManagementPreviewTokens';
	import { getLibraryManagementTagEditorQuery } from '$lib/queries/library-management/LibraryManagementQueries.svelte';
	import type {
		LibraryManagementTagEditMode,
		LibraryManagementTagEditValue,
		LibraryManagementTagEditorField
	} from '$lib/queries/library-management/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { LibraryFileMeta } from '$lib/types';
	import { createUuid } from '$lib/utils/uuid';

	interface Props {
		track: LibraryFileMeta;
		open: boolean;
	}

	let { track, open = $bindable(false) }: Props = $props();
	let dialog: HTMLDialogElement;
	let heading: HTMLHeadingElement;
	let mode = $state<LibraryManagementTagEditMode>('save_override');
	let drafts = $state<Record<string, string>>({});
	let originals = $state<Record<string, string>>({});
	let resetFields = $state<string[]>([]);
	let advancedSearch = $state('');
	let seededTrackRevision = $state<number | null>(null);
	let localError = $state('');

	const contextQuery = getLibraryManagementTagEditorQuery(
		() => authStore.user?.id,
		() => track.id,
		() => open && authStore.isAdmin
	);
	const createPreview = createLibraryManagementTagEditPreviewMutation();
	const context = $derived(contextQuery.data);
	const pending = $derived(createPreview.isPending);
	const commonNames = new Set([
		'title',
		'artist',
		'album',
		'album_artist',
		'date',
		'genre',
		'track_number',
		'total_tracks',
		'disc_number',
		'total_discs'
	]);
	const commonFields = $derived(
		context?.fields.filter((field) => commonNames.has(field.field_name)) ?? []
	);
	const advancedFields = $derived(
		(context?.fields ?? []).filter(
			(field) =>
				!commonNames.has(field.field_name) &&
				(!advancedSearch.trim() ||
					fieldLabel(field.field_name).toLowerCase().includes(advancedSearch.trim().toLowerCase()))
		)
	);
	const overriddenFields = $derived(context?.fields.filter((field) => field.override_id) ?? []);
	const changedFields = $derived(
		(context?.fields ?? []).filter(
			(field) => drafts[field.field_name] !== originals[field.field_name]
		)
	);
	const albumExpansion = $derived(
		mode === 'reset_canonical'
			? overriddenFields.some(
					(field) => field.scope === 'album' && resetFields.includes(field.field_name)
				)
			: changedFields.some((field) => field.scope === 'album')
	);
	const selectionCount = $derived(
		mode === 'reset_canonical' ? resetFields.length : changedFields.length
	);

	$effect(() => {
		if (open) {
			dialog?.showModal();
			queueMicrotask(() => heading?.focus());
		} else if (dialog?.open && !pending) {
			dialog.close();
		}
	});

	$effect(() => {
		if (!context || context.track_revision === seededTrackRevision) return;
		const next = Object.fromEntries(
			context.fields.map((field) => [field.field_name, formatValue(field)])
		);
		drafts = { ...next };
		originals = next;
		resetFields = [];
		mode = 'save_override';
		localError = '';
		seededTrackRevision = context.track_revision;
	});

	function fieldLabel(name: string): string {
		return name
			.split('_')
			.map((part) => (part === 'id' ? 'ID' : part.charAt(0).toUpperCase() + part.slice(1)))
			.join(' ')
			.replace('Musicbrainz', 'MusicBrainz')
			.replace('Isrc', 'ISRC')
			.replace('Asin', 'ASIN');
	}

	function formatValue(field: LibraryManagementTagEditorField): string {
		if (Array.isArray(field.current_value)) return field.current_value.join('\n');
		if (field.current_value === null) return '';
		return String(field.current_value);
	}

	function parsedValue(field: LibraryManagementTagEditorField): LibraryManagementTagEditValue {
		const raw = drafts[field.field_name] ?? '';
		if (field.cardinality === 'ordered_strings') {
			return raw
				.split('\n')
				.map((value) => value.trim())
				.filter(Boolean);
		}
		if (!raw.trim()) return null;
		if (field.cardinality === 'integer') return Number(raw);
		if (field.cardinality === 'boolean') return raw === 'true';
		return raw.trim();
	}

	function chooseMode(value: LibraryManagementTagEditMode) {
		mode = value;
		localError = '';
	}

	function toggleReset(fieldName: string) {
		resetFields = resetFields.includes(fieldName)
			? resetFields.filter((value) => value !== fieldName)
			: [...resetFields, fieldName];
	}

	function close() {
		if (!pending) dialog.close();
	}

	async function preview() {
		if (!context || !context.accepted_identity) return;
		const selected =
			mode === 'reset_canonical'
				? context.fields.filter((field) => resetFields.includes(field.field_name))
				: changedFields;
		if (!selected.length) {
			localError =
				mode === 'reset_canonical'
					? 'Select at least one local override to reset.'
					: 'Change at least one field before creating a preview.';
			return;
		}
		localError = '';
		try {
			const result = await createPreview.mutateAsync({
				local_track_id: context.local_track_id,
				mode,
				expected_settings_revision: context.settings_revision,
				expected_policy_revision: context.policy_revision,
				fields: selected.map((field) => ({
					field_name: field.field_name,
					...(mode === 'reset_canonical' ? {} : { value: parsedValue(field) })
				})),
				idempotency_key: createUuid()
			});
			rememberLibraryManagementPreviewToken(result.job_id, result.preview_token);
			open = false;
			await goto(`/library/management/previews/${encodeURIComponent(result.job_id)}`);
		} catch (error) {
			localError = error instanceof Error ? error.message : 'Could not create the tag preview.';
		}
	}
</script>

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby="tag-editor-title"
	onclose={() => (open = false)}
	oncancel={(event) => {
		if (pending) event.preventDefault();
	}}
>
	<div class="modal-box management-tag-editor max-w-4xl p-0">
		<header class="management-profile-editor__header">
			<div>
				<p class="management-kicker"><Tags class="h-3.5 w-3.5" /> Staged metadata edit</p>
				<h2
					bind:this={heading}
					id="tag-editor-title"
					tabindex="-1"
					class="font-display text-xl font-semibold"
				>
					Edit tags for {track.title}
				</h2>
				<p class="mt-1 text-sm text-base-content/55">
					Changes are previewed, journalled, validated, and published through Library Management.
				</p>
			</div>
			<button
				class="btn btn-ghost btn-sm btn-square"
				aria-label="Close tag editor"
				disabled={pending}
				onclick={close}><X class="h-5 w-5" /></button
			>
		</header>

		<div class="max-h-[72vh] overflow-y-auto p-5 sm:p-6">
			{#if contextQuery.isError}
				<div class="alert alert-error" role="alert">
					Could not load the file's current metadata.
				</div>
			{:else if contextQuery.isPending || !context}
				<div class="grid gap-3" aria-label="Loading tag editor">
					<div class="skeleton h-20 w-full"></div>
					<div class="skeleton h-72 w-full"></div>
				</div>
			{:else}
				<div class="management-write-notice">
					<ShieldAlert class="h-5 w-5" />
					<div>
						<strong>This writes to the audio file.</strong>
						<p>
							No write occurs here. The next page shows the exact before/after diff and requires
							confirmation.
						</p>
					</div>
				</div>

				{#if !context.accepted_identity}
					<div class="alert alert-warning mt-4">
						<ShieldAlert class="h-5 w-5" />
						<span>
							This file needs an accepted MusicBrainz release and track mapping before tags can be
							written ({context.identity_reason?.replaceAll('_', ' ').toLowerCase()}).
						</span>
					</div>
				{/if}

				<section class="mt-5" aria-labelledby="tag-save-behavior">
					<h3 id="tag-save-behavior" class="font-display text-base font-semibold">
						Choose future behavior
					</h3>
					<div class="management-tag-modes mt-2">
						<button
							type="button"
							class:active={mode === 'save_override'}
							aria-pressed={mode === 'save_override'}
							onclick={() => chooseMode('save_override')}
						>
							<Check class="h-4 w-4" />
							<span
								><strong>Save as local override</strong><small
									>Recommended. Future management preserves your value.</small
								></span
							>
						</button>
						<button
							type="button"
							class:active={mode === 'write_once'}
							aria-pressed={mode === 'write_once'}
							onclick={() => chooseMode('write_once')}
						>
							<ListPlus class="h-4 w-4" />
							<span
								><strong>Write once</strong><small
									>A later authoritative run may replace this value.</small
								></span
							>
						</button>
						<button
							type="button"
							class:active={mode === 'reset_canonical'}
							aria-pressed={mode === 'reset_canonical'}
							onclick={() => chooseMode('reset_canonical')}
						>
							<RotateCcw class="h-4 w-4" />
							<span
								><strong>Reset to canonical</strong><small
									>Remove selected overrides and preview MusicBrainz values.</small
								></span
							>
						</button>
					</div>
				</section>

				{#if mode === 'reset_canonical'}
					<section class="mt-5" aria-labelledby="tag-reset-fields">
						<h3 id="tag-reset-fields" class="font-display text-base font-semibold">
							Select overrides to reset
						</h3>
						{#if overriddenFields.length}
							<div class="mt-2 grid gap-2 sm:grid-cols-2">
								{#each overriddenFields as field (field.field_name)}
									<label class="management-selection-card">
										<input
											type="checkbox"
											class="checkbox checkbox-sm"
											checked={resetFields.includes(field.field_name)}
											onchange={() => toggleReset(field.field_name)}
										/>
										<span
											><strong>{fieldLabel(field.field_name)}</strong><small
												>{field.scope}-level override</small
											></span
										>
									</label>
								{/each}
							</div>
						{:else}
							<p class="mt-2 rounded-box bg-base-200/50 p-4 text-sm text-base-content/60">
								This track has no local overrides to reset.
							</p>
						{/if}
					</section>
				{:else}
					<section class="mt-5" aria-labelledby="tag-core-fields">
						<div class="flex items-end justify-between gap-3">
							<div>
								<h3 id="tag-core-fields" class="font-display text-base font-semibold">Core tags</h3>
								<p class="text-xs text-base-content/50">
									Artists and genres use one value per line.
								</p>
							</div>
							<span class="badge badge-ghost badge-sm">{changedFields.length} changed</span>
						</div>
						<div class="management-tag-fields mt-3">
							{#each commonFields as field (field.field_name)}
								{@render fieldControl(field)}
							{/each}
						</div>
					</section>

					<details class="management-disclosure mt-5">
						<summary><span>Advanced managed fields</span><ChevronDown class="h-4 w-4" /></summary>
						<div class="p-4">
							<label class="grid gap-1 text-sm">
								<span>Find a field</span>
								<input
									class="input input-bordered input-sm"
									bind:value={advancedSearch}
									placeholder="Composer, ISRC, sort name…"
								/>
							</label>
							<div class="management-tag-fields mt-3">
								{#each advancedFields as field (field.field_name)}
									{@render fieldControl(field)}
								{/each}
							</div>
						</div>
					</details>
				{/if}

				{#if albumExpansion}
					<div class="alert alert-info mt-4 text-sm">
						Album-level fields apply consistently to every mapped track in this album. The preview
						will show that expansion before anything is written.
					</div>
				{/if}
				{#if localError}<div class="alert alert-error mt-4 text-sm">{localError}</div>{/if}
			{/if}
		</div>

		<footer class="management-profile-editor__footer">
			<p class="text-xs text-base-content/50">
				Using {context?.profile_name ?? 'the root profile'} format compatibility rules.
			</p>
			<div class="flex gap-2">
				<button class="btn btn-ghost" disabled={pending} onclick={close}>Cancel</button>
				<button
					class="btn management-btn"
					disabled={pending || !context?.accepted_identity || selectionCount === 0}
					onclick={preview}
				>
					{#if pending}<span class="loading loading-spinner loading-sm"></span>{/if}
					Preview {selectionCount || ''}
					{selectionCount === 1 ? 'change' : 'changes'}
				</button>
			</div>
		</footer>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close tag editor" disabled={pending}>close</button>
	</form>
</dialog>

{#snippet fieldControl(field: LibraryManagementTagEditorField)}
	<label
		class="management-tag-field"
		class:changed={drafts[field.field_name] !== originals[field.field_name]}
	>
		<span>
			<strong>{fieldLabel(field.field_name)}</strong>
			<small>{field.scope} · {field.cardinality.replace('_', ' ')}</small>
		</span>
		{#if field.override_id}<span class="badge badge-warning badge-outline badge-xs"
				>Local override</span
			>{/if}
		{#if field.cardinality === 'ordered_strings'}
			<textarea
				class="textarea textarea-bordered min-h-20 sm:col-span-2"
				bind:value={drafts[field.field_name]}
			></textarea>
		{:else if field.cardinality === 'boolean'}
			<select
				class="select select-bordered select-sm sm:col-span-2"
				bind:value={drafts[field.field_name]}
			>
				<option value="">Not set</option>
				<option value="true">Yes</option>
				<option value="false">No</option>
			</select>
		{:else}
			<input
				type={field.cardinality === 'integer' ? 'number' : 'text'}
				min={field.cardinality === 'integer' ? 0 : undefined}
				class="input input-bordered input-sm sm:col-span-2"
				bind:value={drafts[field.field_name]}
			/>
		{/if}
	</label>
{/snippet}
