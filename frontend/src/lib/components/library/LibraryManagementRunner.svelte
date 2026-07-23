<script lang="ts">
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import {
		ArrowLeft,
		ArrowRight,
		Check,
		FolderCog,
		Search,
		ShieldAlert,
		Sparkles,
		X
	} from 'lucide-svelte';

	import { getLibrarySearchQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import type { LibraryRootSettings } from '$lib/queries/library/LibraryOperationsTypes';
	import {
		createLibraryManagementBaselineRestorePreviewMutation,
		createLibraryManagementPreviewMutation
	} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import { rememberLibraryManagementPreviewToken } from '$lib/queries/library-management/LibraryManagementPreviewTokens';
	import type {
		LibraryManagementProfile,
		LibraryManagementRootOverrides,
		LibraryManagementSelection,
		LibraryManagementSettingsResponse,
		ManagementSelectionKind
	} from '$lib/queries/library-management/types';
	import { createUuid } from '$lib/utils/uuid';

	interface Props {
		mode?: 'manage' | 'baseline_restore';
		roots: LibraryRootSettings[];
		settings: LibraryManagementSettingsResponse;
		policyRevision: string;
		onclose: () => void;
	}

	let { mode = 'manage', roots, settings, policyRevision, onclose }: Props = $props();
	let dialog: HTMLDialogElement;
	let heading: HTMLHeadingElement;
	let step = $state(1);
	let selectionKind = $state<ManagementSelectionKind>('roots');
	const initialRootIds = (): string[] => roots.map((root) => root.id);
	let selectedIds = $state<string[]>(initialRootIds());
	let searchTerm = $state('');
	let filterSearch = $state('');
	let filterGenre = $state('');
	let filterFromYear = $state<number | null>(null);
	let filterToYear = $state<number | null>(null);
	const initialProfileId = (): string => settings.default_profile_id;
	let profileId = $state(initialProfileId());
	let customized = $state(false);
	let metadataEnabled = $state(true);
	let genresEnabled = $state(true);
	let embeddedArtworkEnabled = $state(true);
	let externalArtworkEnabled = $state(true);
	let renameEnabled = $state(true);
	let moveEnabled = $state(true);
	let sidecarsEnabled = $state(true);
	let targetRootId = $state('');
	let seededProfileId = $state('');
	let localError = $state('');

	const searchQuery = getLibrarySearchQuery(() => searchTerm);
	const createPreview = createLibraryManagementPreviewMutation();
	const createRestore = createLibraryManagementBaselineRestorePreviewMutation();
	const profile = $derived(settings.profiles.find((value) => value.id === profileId) ?? null);
	const selectionValid = $derived(
		selectionKind === 'filter'
			? filterFromYear === null || filterToYear === null || filterFromYear <= filterToYear
			: selectedIds.length > 0
	);
	const expansionRequired = $derived(
		selectionKind === 'tracks' && (renameEnabled || moveEnabled || sidecarsEnabled)
	);
	const pending = $derived(createPreview.isPending || createRestore.isPending);

	$effect(() => {
		if (!profile || profile.id === seededProfileId) return;
		metadataEnabled = profile.metadata.enabled;
		genresEnabled = profile.genres.enabled;
		embeddedArtworkEnabled = profile.artwork.embedded_enabled;
		externalArtworkEnabled = profile.artwork.external_enabled;
		renameEnabled = profile.organization.rename_enabled;
		moveEnabled = profile.organization.move_enabled;
		sidecarsEnabled = profile.organization.move_sidecars;
		seededProfileId = profile.id;
	});

	onMount(() => {
		dialog.showModal();
		heading.focus();
	});

	function selectKind(kind: ManagementSelectionKind): void {
		selectionKind = kind;
		searchTerm = '';
		selectedIds = kind === 'roots' ? roots.map((root) => root.id) : [];
	}

	function toggleId(id: string): void {
		selectedIds = selectedIds.includes(id)
			? selectedIds.filter((value) => value !== id)
			: [...selectedIds, id];
	}

	function resultItems(): Array<{ id: string; title: string; subtitle: string }> {
		if (!searchQuery.data) return [];
		if (selectionKind === 'artists') {
			return searchQuery.data.artists.map((artist) => ({
				id: artist.id,
				title: artist.name,
				subtitle: `${artist.album_count} albums · ${artist.track_count} tracks`
			}));
		}
		if (selectionKind === 'albums') {
			return searchQuery.data.albums.map((album) => ({
				id: album.id,
				title: album.title,
				subtitle: `${album.artist_name} · ${album.track_count} tracks`
			}));
		}
		if (selectionKind === 'tracks') {
			return searchQuery.data.tracks.map((track) => ({
				id: track.id,
				title: track.title,
				subtitle: `${track.artist_name} · ${track.album_title}`
			}));
		}
		return [];
	}

	function selection(): LibraryManagementSelection {
		if (selectionKind === 'filter') {
			return {
				kind: 'filter',
				catalog_filter: {
					search: filterSearch.trim() || null,
					genre: filterGenre.trim() || null,
					from_year: filterFromYear,
					to_year: filterToYear,
					artist_ids: [],
					album_artist_only: false
				}
			};
		}
		return { kind: selectionKind, ids: selectedIds };
	}

	function overrides(): LibraryManagementRootOverrides | null {
		if (!customized) return null;
		return {
			metadata_enabled: metadataEnabled,
			genres_enabled: genresEnabled,
			embedded_artwork_enabled: embeddedArtworkEnabled,
			external_artwork_enabled: externalArtworkEnabled,
			rename_enabled: renameEnabled,
			move_enabled: moveEnabled,
			move_sidecars: sidecarsEnabled,
			source_cleanup: null,
			preserve_timestamps: null,
			naming_script_id: null
		};
	}

	function next(): void {
		if (!selectionValid) return;
		if (mode === 'baseline_restore' && step === 1) {
			step = 4;
			return;
		}
		step = Math.min(4, step + 1);
	}

	function back(): void {
		if (mode === 'baseline_restore' && step === 4) {
			step = 1;
			return;
		}
		step = Math.max(1, step - 1);
	}

	async function generate(): Promise<void> {
		const currentProfile = profile;
		if (!selectionValid || (mode === 'manage' && !currentProfile)) return;
		localError = '';
		try {
			const handle =
				mode === 'baseline_restore'
					? await createRestore.mutateAsync({
							selection: selection(),
							expected_settings_revision: settings.settings_revision,
							expected_policy_revision: policyRevision,
							idempotency_key: createUuid()
						})
					: await createPreview.mutateAsync({
							selection: selection(),
							profile_id: currentProfile?.id ?? '',
							expected_settings_revision: settings.settings_revision,
							expected_policy_revision: policyRevision,
							idempotency_key: createUuid(),
							target_root_id: targetRootId || null,
							overrides: overrides()
						});
			rememberLibraryManagementPreviewToken(handle.job_id, handle.preview_token);
			dialog.close();
			await goto(`/library/management/previews/${encodeURIComponent(handle.job_id)}`);
		} catch (error) {
			localError = error instanceof Error ? error.message : 'Could not create the preview.';
		}
	}

	function scopeLabel(): string {
		if (selectionKind === 'filter') return 'Current catalog filter';
		if (selectionKind === 'roots' && selectedIds.length === roots.length)
			return 'All library roots';
		return `${selectedIds.length} selected ${selectionKind}`;
	}

	function profileWork(value: LibraryManagementProfile): string[] {
		const work: string[] = [];
		if (customized ? metadataEnabled : value.metadata.enabled) work.push('tags');
		if (customized ? genresEnabled : value.genres.enabled) work.push('genres');
		if (
			customized
				? embeddedArtworkEnabled || externalArtworkEnabled
				: value.artwork.embedded_enabled || value.artwork.external_enabled
		)
			work.push('artwork');
		if (customized ? renameEnabled : value.organization.rename_enabled) work.push('rename');
		if (customized ? moveEnabled : value.organization.move_enabled) work.push('move');
		if (customized ? sidecarsEnabled : value.organization.move_sidecars) work.push('sidecars');
		return work;
	}
</script>

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby="management-runner-title"
	{onclose}
	oncancel={(event) => {
		if (pending) event.preventDefault();
	}}
>
	<div class="modal-box management-runner max-w-3xl p-0">
		<header class="management-profile-editor__header">
			<div>
				<p class="management-kicker"><FolderCog class="h-3.5 w-3.5" /> Manual write planning</p>
				<h2
					bind:this={heading}
					id="management-runner-title"
					tabindex="-1"
					class="font-display text-xl font-semibold"
				>
					{mode === 'baseline_restore'
						? 'Restore first-management baselines'
						: 'Preview Library Management'}
				</h2>
				<p class="mt-1 text-sm text-base-content/55">
					This creates a read-only durable preview. It does not change a music file.
				</p>
			</div>
			<button
				class="btn btn-ghost btn-sm btn-square"
				aria-label="Close manual management runner"
				disabled={pending}
				onclick={() => dialog.close()}><X class="h-5 w-5" /></button
			>
		</header>

		<div class="management-runner-progress" aria-label="Runner progress">
			{#each [1, 2, 3, 4] as value (value)}
				<span data-state={step === value ? 'current' : step > value ? 'done' : 'waiting'}
					>{#if step > value}<Check class="h-3.5 w-3.5" />{:else}{value}{/if}<small
						>{['Scope', 'Profile', 'Work', 'Review'][value - 1]}</small
					></span
				>
			{/each}
		</div>

		<div class="max-h-[65vh] min-h-80 overflow-y-auto p-5 sm:p-6">
			{#if step === 1}
				<section class="space-y-4">
					<div>
						<h3 class="font-display text-lg font-semibold">Choose scope</h3>
						<p class="text-sm text-base-content/55">
							Album organization expands selected tracks to complete album bundles before planning.
						</p>
					</div>
					<div class="management-scope-tabs" role="tablist" aria-label="Management scope type">
						{#each [{ value: 'roots', label: 'Roots' }, { value: 'artists', label: 'Artists' }, { value: 'albums', label: 'Albums' }, { value: 'tracks', label: 'Tracks' }, { value: 'filter', label: 'Catalog filter' }] as option (option.value)}
							<button
								role="tab"
								aria-selected={selectionKind === option.value}
								onclick={() => selectKind(option.value as ManagementSelectionKind)}
								>{option.label}</button
							>
						{/each}
					</div>
					{#if selectionKind === 'roots'}
						<div class="grid gap-2 sm:grid-cols-2">
							{#each roots as root (root.id)}<label class="management-selection-card"
									><input
										type="checkbox"
										class="checkbox checkbox-sm"
										checked={selectedIds.includes(root.id)}
										onchange={() => toggleId(root.id)}
									/><span
										><strong>{root.label}</strong><small
											>{root.policy.replaceAll('_', ' ')} scanning policy</small
										></span
									></label
								>{/each}
						</div>
					{:else if selectionKind === 'filter'}
						<div class="grid gap-3 sm:grid-cols-2">
							<label class="grid gap-1 text-sm sm:col-span-2"
								><span>Catalog search</span><input
									class="input input-bordered bg-base-100"
									bind:value={filterSearch}
									placeholder="Artist, album, or title"
								/></label
							>
							<label class="grid gap-1 text-sm"
								><span>Genre</span><input
									class="input input-bordered bg-base-100"
									bind:value={filterGenre}
								/></label
							>
							<div class="grid grid-cols-2 gap-2">
								<label class="grid gap-1 text-sm"
									><span>From year</span><input
										type="number"
										class="input input-bordered bg-base-100"
										bind:value={filterFromYear}
									/></label
								><label class="grid gap-1 text-sm"
									><span>To year</span><input
										type="number"
										class="input input-bordered bg-base-100"
										bind:value={filterToYear}
									/></label
								>
							</div>
						</div>
					{:else}
						<label class="input input-bordered flex items-center gap-2 bg-base-100"
							><Search class="h-4 w-4 text-base-content/40" /><input
								class="grow"
								bind:value={searchTerm}
								placeholder={`Search library ${selectionKind}`}
								aria-label={`Search library ${selectionKind}`}
							/></label
						>
						{#if searchTerm.trim().length < 2}<p class="text-sm text-base-content/45">
								Type at least two characters, then select one or more results.
							</p>{:else if searchQuery.isLoading}<div class="space-y-2">
								<div class="skeleton h-14"></div>
								<div class="skeleton h-14"></div>
							</div>{:else if searchQuery.isError}<div
								class="alert alert-error text-sm"
								role="alert"
							>
								Could not search the library. Check the connection and try again.
							</div>{:else if resultItems().length === 0}<p
								class="rounded-xl border border-dashed border-base-content/15 p-4 text-sm text-base-content/50"
							>
								No matching {selectionKind} found.
							</p>{:else}<div class="grid gap-2">
								{#each resultItems() as item (item.id)}<label class="management-selection-card"
										><input
											type="checkbox"
											class="checkbox checkbox-sm"
											checked={selectedIds.includes(item.id)}
											onchange={() => toggleId(item.id)}
										/><span><strong>{item.title}</strong><small>{item.subtitle}</small></span
										></label
									>{/each}
							</div>{/if}
					{/if}
					{#if !selectionValid}<p class="text-sm text-error" role="alert">
							Choose at least one item and keep the year range valid.
						</p>{/if}
				</section>
			{:else if step === 2}
				<section class="space-y-4">
					<div>
						<h3 class="font-display text-lg font-semibold">Choose profile</h3>
						<p class="text-sm text-base-content/55">
							The profile is pinned into the preview. Editing it later makes this preview stale.
						</p>
					</div>
					<div class="grid gap-2">
						{#each settings.profiles as option (option.id)}<label class="management-selection-card"
								><input
									type="radio"
									name="management-profile"
									class="radio radio-sm"
									value={option.id}
									bind:group={profileId}
								/><span><strong>{option.name}</strong><small>{option.description}</small></span
								></label
							>{/each}
					</div>
				</section>
			{:else if step === 3 && profile}
				<section class="space-y-4">
					<div>
						<h3 class="font-display text-lg font-semibold">Choose work</h3>
						<p class="text-sm text-base-content/55">
							Use the profile unchanged or make temporary one-run choices. The saved profile is
							never edited.
						</p>
					</div>
					<label class="management-master-toggle"
						><input type="checkbox" class="toggle toggle-sm" bind:checked={customized} /><span
							><strong>Customize this run</strong><small
								>Temporary values are pinned only to this preview.</small
							></span
						></label
					>{#if customized}<div class="grid gap-2 sm:grid-cols-2">
							{#each [{ label: 'Metadata tags', get: () => metadataEnabled, set: (value: boolean) => (metadataEnabled = value) }, { label: 'Genres', get: () => genresEnabled, set: (value: boolean) => (genresEnabled = value) }, { label: 'Embedded artwork', get: () => embeddedArtworkEnabled, set: (value: boolean) => (embeddedArtworkEnabled = value) }, { label: 'External artwork', get: () => externalArtworkEnabled, set: (value: boolean) => (externalArtworkEnabled = value) }, { label: 'Rename files', get: () => renameEnabled, set: (value: boolean) => (renameEnabled = value) }, { label: 'Move within root', get: () => moveEnabled, set: (value: boolean) => (moveEnabled = value) }, { label: 'Move sidecars', get: () => sidecarsEnabled, set: (value: boolean) => (sidecarsEnabled = value) }] as option (option.label)}<label
									class="management-trigger"
									><input
										type="checkbox"
										class="checkbox checkbox-sm"
										checked={option.get()}
										onchange={(event) => option.set(event.currentTarget.checked)}
									/><span><strong>{option.label}</strong><small>One run only</small></span></label
								>{/each}
						</div>{/if}<label class="grid gap-1 text-sm"
						><span>Explicit cross-root destination</span><select
							class="select select-bordered bg-base-100"
							bind:value={targetRootId}
							disabled={!moveEnabled}
							><option value="">Keep organization within each source root</option
							>{#each roots.filter((root) => selectionKind !== 'roots' || !selectedIds.includes(root.id)) as root (root.id)}<option
									value={root.id}>{root.label}</option
								>{/each}</select
						><small class="text-base-content/45"
							>Cross-root movement happens only in this manual preview and is never an automatic
							setting.</small
						></label
					>
				</section>
			{:else if step === 4}
				<section class="space-y-4">
					<div>
						<h3 class="font-display text-lg font-semibold">Review expansion</h3>
						<p class="text-sm text-base-content/55">
							Generating the preview performs reads and durable planning only.
						</p>
					</div>
					<div class="management-review-grid">
						<div><span>Scope</span><strong>{scopeLabel()}</strong></div>
						<div>
							<span>Profile</span><strong
								>{mode === 'baseline_restore'
									? 'Original first-management state'
									: profile?.name}</strong
							>
						</div>
						<div>
							<span>Work</span><strong
								>{mode === 'baseline_restore'
									? 'Restore tags, art, sidecars, and original paths'
									: profile
										? profileWork(profile).join(', ') || 'No changes enabled'
										: '-'}</strong
							>
						</div>
						<div>
							<span>Destination</span><strong
								>{targetRootId
									? roots.find((root) => root.id === targetRootId)?.label
									: 'Within source root'}</strong
							>
						</div>
					</div>
					{#if expansionRequired}<div class="alert alert-warning items-start">
							<ShieldAlert class="mt-0.5 h-5 w-5" /><span
								>Track selection expands to complete albums because organization or sidecar work
								must remain atomic.</span
							>
						</div>{/if}{#if mode === 'baseline_restore'}<div
							class="alert alert-warning items-start"
						>
							<ShieldAlert class="mt-0.5 h-5 w-5" /><span
								>Restore means “how files were before DroppedNeedle first managed them.” It is
								separate from Undo, and restored files remain unmanaged until deliberately enabled
								again.</span
							>
						</div>{/if}
					<p class="text-sm text-base-content/55">
						A later page shows every eligible, warning, blocked, preserved, and no-change item
						before any Apply button exists.
					</p>
				</section>
			{/if}
			{#if localError}<div class="alert alert-error mt-4 text-sm" role="alert">
					{localError}
				</div>{/if}
		</div>

		<footer class="management-profile-editor__footer">
			<button class="btn btn-ghost" disabled={step === 1 || pending} onclick={back}
				><ArrowLeft class="h-4 w-4" /> Back</button
			>
			<div class="flex gap-2">
				<button class="btn btn-ghost" disabled={pending} onclick={() => dialog.close()}
					>Cancel</button
				>{#if step < 4}<button class="btn management-btn" disabled={!selectionValid} onclick={next}
						>Continue <ArrowRight class="h-4 w-4" /></button
					>{:else}<button
						class="btn management-btn"
						disabled={!selectionValid || pending || (mode === 'manage' && !profile)}
						onclick={() => void generate()}
						>{#if pending}<span class="loading loading-spinner loading-sm"></span>{/if}<Sparkles
							class="h-4 w-4"
						/> Generate preview</button
					>{/if}
			</div>
		</footer>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close management runner" disabled={pending}>close</button>
	</form>
</dialog>
