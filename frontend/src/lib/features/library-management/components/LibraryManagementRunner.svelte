<script lang="ts">
	import '$lib/features/library-management/components/library-management.css';
	import { goto } from '$app/navigation';
	import { ArrowLeft, ArrowRight, Check, Search, ShieldAlert, Sparkles, X } from 'lucide-svelte';

	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import { getLibrarySearchQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import type { LibraryRootSettings } from '$lib/features/library-management/queries/LibraryOperationsTypes';
	import {
		createLibraryManagementBaselineRestorePreviewMutation,
		createLibraryManagementPreviewMutation
	} from '$lib/features/library-management/queries/LibraryManagementMutations.svelte';
	import { rememberLibraryManagementPreviewToken } from '$lib/features/library-management/queries/LibraryManagementPreviewTokens';
	import type {
		LibraryManagementProfile,
		LibraryManagementRootOverrides,
		LibraryManagementSelection,
		LibraryManagementSettingsResponse,
		ManagementSelectionKind
	} from '$lib/features/library-management/queries/types';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { createUuid } from '$lib/utils/uuid';

	interface Props {
		mode?: 'manage' | 'baseline_restore';
		roots: LibraryRootSettings[];
		settings: LibraryManagementSettingsResponse;
		policyRevision: string;
		onclose: () => void;
	}

	interface SelectedScopeItem {
		id: string;
		title: string;
		subtitle: string;
		image: {
			kind: 'album' | 'artist';
			/** Local album id for album art; unused for artists. */
			id: string;
			/** Release group for albums, the artist's own MBID for artists. */
			mbid: string;
			available: boolean;
		} | null;
	}

	let { mode = 'manage', roots, settings, policyRevision, onclose }: Props = $props();
	let shown = $state(true);
	let step = $state(1);
	let selectionKind = $state<ManagementSelectionKind>('roots');
	const initialRootIds = (): string[] => roots.map((root) => root.id);
	let selectedIds = $state<string[]>(initialRootIds());
	let selectedItems = $state<SelectedScopeItem[]>([]);
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
	const filterHasCriteria = $derived(
		Boolean(
			filterSearch.trim() || filterGenre.trim() || filterFromYear !== null || filterToYear !== null
		)
	);
	const destinationOptions = $derived([
		{ value: '', label: 'Keep organization within each source root' },
		...roots
			.filter((root) => selectionKind !== 'roots' || !selectedIds.includes(root.id))
			.map((root) => ({ value: root.id, label: root.label }))
	]);
	const progressSteps = $derived(
		mode === 'baseline_restore'
			? [
					{ step: 1, number: 1, label: 'Scope' },
					{ step: 4, number: 2, label: 'Review' }
				]
			: [
					{ step: 1, number: 1, label: 'Scope' },
					{ step: 2, number: 2, label: 'Profile' },
					{ step: 3, number: 3, label: 'Work' },
					{ step: 4, number: 4, label: 'Review' }
				]
	);

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

	function close(): void {
		shown = false;
		onclose();
	}

	function selectKind(kind: ManagementSelectionKind): void {
		selectionKind = kind;
		searchTerm = '';
		selectedIds = kind === 'roots' ? roots.map((root) => root.id) : [];
		selectedItems = [];
	}

	function toggleId(id: string): void {
		selectedIds = selectedIds.includes(id)
			? selectedIds.filter((value) => value !== id)
			: [...selectedIds, id];
	}

	function toggleResult(item: SelectedScopeItem): void {
		if (selectedIds.includes(item.id)) {
			removeSelected(item.id);
			return;
		}
		selectedIds = [...selectedIds, item.id];
		selectedItems = [...selectedItems, item];
	}

	function removeSelected(id: string): void {
		selectedIds = selectedIds.filter((value) => value !== id);
		selectedItems = selectedItems.filter((value) => value.id !== id);
	}

	function clearSelection(): void {
		selectedIds = [];
		selectedItems = [];
	}

	function quantity(value: number, singular: string, plural = `${singular}s`): string {
		return `${value.toLocaleString()} ${value === 1 ? singular : plural}`;
	}

	function selectionNoun(kind: ManagementSelectionKind, count = 2): string {
		const nouns: Record<ManagementSelectionKind, [string, string]> = {
			roots: ['root', 'roots'],
			artists: ['artist', 'artists'],
			albums: ['release', 'releases'],
			tracks: ['track', 'tracks'],
			filter: ['catalog filter', 'catalog filters']
		};
		return nouns[kind][count === 1 ? 0 : 1];
	}

	function currentScopeItems(): SelectedScopeItem[] {
		if (selectionKind === 'roots') {
			return roots
				.filter((root) => selectedIds.includes(root.id))
				.map((root) => ({
					id: root.id,
					title: root.label,
					subtitle: `${root.policy.replaceAll('_', ' ')} scanning policy`,
					image: null
				}));
		}
		return selectedItems;
	}

	function resultItems(): SelectedScopeItem[] {
		if (!searchQuery.data) return [];
		if (selectionKind === 'artists') {
			return searchQuery.data.artists.map((artist) => ({
				id: artist.id,
				title: artist.name,
				subtitle: `${quantity(artist.album_count, 'release')} · ${quantity(artist.led_track_count, 'track')}`,
				image: {
					kind: 'artist',
					id: artist.id,
					mbid: artist.artist_mbid ?? '',
					available: Boolean(artist.artist_mbid)
				}
			}));
		}
		if (selectionKind === 'albums') {
			return searchQuery.data.albums.map((album) => ({
				id: album.id,
				title: album.title,
				subtitle: `${album.artist_name} · ${quantity(album.track_count, 'track')}`,
				image: {
					kind: 'album',
					id: album.id,
					mbid: album.release_group_mbid ?? '',
					available: album.cover_available
				}
			}));
		}
		if (selectionKind === 'tracks') {
			return searchQuery.data.tracks.map((track) => ({
				id: track.id,
				title: track.title,
				subtitle: `${track.artist_name} · ${track.album_title}`,
				image: {
					kind: 'album',
					id: track.album_id,
					mbid: track.release_group_mbid ?? '',
					available: track.cover_available
				}
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
			naming_script_id: null,
			multi_disc_naming_mode: 'inherit',
			multi_disc_naming_script_id: null
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
			close();
			await goto(withBasePath(`/library/management/previews/${encodeURIComponent(handle.job_id)}`));
		} catch (error) {
			localError = error instanceof Error ? error.message : 'Could not create the preview.';
		}
	}

	function scopeLabel(): string {
		if (selectionKind === 'filter') {
			return filterHasCriteria ? 'Current catalog filter' : 'Entire library catalog';
		}
		if (selectionKind === 'roots' && selectedIds.length === roots.length)
			return 'All library roots';
		return `${selectedIds.length.toLocaleString()} selected ${selectionNoun(selectionKind, selectedIds.length)}`;
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

<Modal
	bind:open={shown}
	size="sheet"
	title={mode === 'baseline_restore' ? 'Restore original state' : 'Preview file organization'}
	description="This creates a saved read-only preview. No music file changes."
	focusTitle
	dismissible={!pending}
	{onclose}
>
	<div class="flex flex-col md:h-full md:min-h-0">
		<div class="management-runner-progress" aria-label="Runner progress">
			{#each progressSteps as value (value.step)}
				<span data-state={step === value.step ? 'current' : step > value.step ? 'done' : 'waiting'}
					>{#if step > value.step}<Check class="h-3.5 w-3.5" />{:else}{value.number}{/if}<small
						>{value.label}</small
					></span
				>
			{/each}
		</div>
		<div class="min-h-80 p-4 md:min-h-0 md:flex-1 md:overflow-y-auto md:p-6">
			{#if step === 1}
				<section class="space-y-4">
					<div>
						<h3 class="font-display text-lg font-semibold">Choose scope</h3>
						<p class="text-sm text-base-content/55">
							Release organization expands selected tracks to complete release bundles before
							planning.
						</p>
					</div>
					<div class="management-scope-tabs" role="group" aria-label="Management scope type">
						{#each [{ value: 'roots', label: 'Roots' }, { value: 'artists', label: 'Artists' }, { value: 'albums', label: 'Releases' }, { value: 'tracks', label: 'Tracks' }, { value: 'filter', label: 'Catalog filter' }] as option (option.value)}
							<button
								aria-pressed={selectionKind === option.value}
								onclick={() => selectKind(option.value as ManagementSelectionKind)}
								>{option.label}</button
							>
						{/each}
					</div>
					{#if selectionKind === 'filter'}
						<div class="management-selected-scope" data-wide-scope={!filterHasCriteria}>
							<div>
								<strong
									>{filterHasCriteria ? 'Filtered catalog scope' : 'Entire library catalog'}</strong
								>
								<small
									>{filterHasCriteria
										? 'The filters below are combined for this preview.'
										: 'No filters are set, so every cataloged file is in scope.'}</small
								>
							</div>
						</div>
					{:else}
						<div
							class="management-selected-scope"
							role="region"
							aria-label="Selected management scope"
						>
							<div class="management-selected-scope__header">
								<div>
									<strong>Current selection</strong>
									<small
										>{currentScopeItems().length.toLocaleString()}
										{currentScopeItems().length === 1 ? 'item' : 'items'} in scope</small
									>
								</div>
								{#if currentScopeItems().length > 0}<Button
										variant="ghost"
										size="xs"
										onclick={clearSelection}>Clear all</Button
									>{/if}
							</div>
							{#if currentScopeItems().length === 0}
								<p class="text-sm text-base-content/45">Nothing is selected yet.</p>
							{:else}
								<div class="management-selected-scope__items">
									{#each currentScopeItems() as item (item.id)}
										<div class="management-selected-scope__item">
											{#if item.image?.kind === 'album'}
												<AlbumImage
													albumId={item.image.id}
													mbid={item.image.mbid}
													available={item.image.available}
													alt=""
													size="xs"
													rounded="md"
													className="h-9 w-9 border border-base-content/10"
													testId={`selected-scope-artwork-${item.id}`}
												/>
											{:else if item.image?.kind === 'artist'}
												<ArtistImage
													mbid={item.image.mbid}
													available={item.image.available}
													alt=""
													size="xs"
													className="h-9 w-9 border border-base-content/10"
												/>
											{/if}
											<span class="min-w-0 flex-1"
												><strong>{item.title}</strong><small>{item.subtitle}</small></span
											>
											<IconButton
												size="xs"
												shape="square"
												icon={X}
												label={`Remove ${item.title} from scope`}
												onclick={() => removeSelected(item.id)}
											/>
										</div>
									{/each}
								</div>
							{/if}
						</div>
					{/if}
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
								><span>Catalog search</span><Input
									label="Catalog search"
									class="w-full"
									bind:value={filterSearch}
									placeholder="Artist, release, or title"
								/></label
							>
							<label class="grid gap-1 text-sm"
								><span>Genre</span><Input
									label="Genre"
									class="w-full"
									bind:value={filterGenre}
								/></label
							>
							<div class="grid grid-cols-2 gap-2">
								<label class="grid gap-1 text-sm"
									><span>From year</span><Input
										type="number"
										label="From year"
										class="w-full"
										bind:value={filterFromYear}
									/></label
								><label class="grid gap-1 text-sm"
									><span>To year</span><Input
										type="number"
										label="To year"
										class="w-full"
										bind:value={filterToYear}
									/></label
								>
							</div>
						</div>
					{:else}
						<Input
							icon={Search}
							class="w-full"
							label={`Search library ${selectionNoun(selectionKind)}`}
							placeholder={`Search library ${selectionNoun(selectionKind)}`}
							bind:value={searchTerm}
						/>
						{#if searchTerm.trim().length < 2}<p class="text-sm text-base-content/45">
								Type at least two characters, then select one or more results.
							</p>{:else if searchQuery.isLoading}<div
								class="space-y-2"
								role="status"
								aria-label="Searching the library"
							>
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
								No matching {selectionNoun(selectionKind)} found.
							</p>{:else}<div class="grid gap-2">
								{#each resultItems() as item (item.id)}<label class="management-selection-card"
										><input
											type="checkbox"
											class="checkbox checkbox-sm"
											checked={selectedIds.includes(item.id)}
											onchange={() => toggleResult(item)}
										/>{#if item.image?.kind === 'album'}<AlbumImage
												albumId={item.image.id}
												mbid={item.image.mbid}
												available={item.image.available}
												alt=""
												size="xs"
												rounded="md"
												className="h-11 w-11 border border-base-content/10"
												testId={`search-scope-artwork-${item.id}`}
											/>{:else if item.image?.kind === 'artist'}<ArtistImage
												mbid={item.image.mbid}
												available={item.image.available}
												alt=""
												size="xs"
												className="h-11 w-11 border border-base-content/10"
											/>{/if}<span class="min-w-0 flex-1"
											><strong>{item.title}</strong><small>{item.subtitle}</small></span
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
					<div
						class="management-runner-profile-list"
						role="radiogroup"
						aria-label="Available management profiles"
						tabindex="0"
					>
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
					<ToggleRow
						label="Customize this run"
						description="Temporary values are pinned only to this preview."
						bind:checked={customized}
					/>{#if customized}<div class="grid gap-2 sm:grid-cols-2">
							{#each [{ label: 'Metadata tags', get: () => metadataEnabled, set: (value: boolean) => (metadataEnabled = value) }, { label: 'Genres', get: () => genresEnabled, set: (value: boolean) => (genresEnabled = value) }, { label: 'Embedded artwork', get: () => embeddedArtworkEnabled, set: (value: boolean) => (embeddedArtworkEnabled = value) }, { label: 'External artwork', get: () => externalArtworkEnabled, set: (value: boolean) => (externalArtworkEnabled = value) }, { label: 'Rename files', get: () => renameEnabled, set: (value: boolean) => (renameEnabled = value) }, { label: 'Move within root', get: () => moveEnabled, set: (value: boolean) => (moveEnabled = value) }, { label: 'Move sidecars', get: () => sidecarsEnabled, set: (value: boolean) => (sidecarsEnabled = value) }] as option (option.label)}<label
									class="management-trigger"
									><input
										type="checkbox"
										class="checkbox checkbox-sm"
										checked={option.get()}
										onchange={(event) => option.set(event.currentTarget.checked)}
									/><span><strong>{option.label}</strong><small>One run only</small></span></label
								>{/each}
						</div>{/if}<SelectField
						label="Explicit cross-root destination"
						help="Cross-root movement happens only in this manual preview and is never an automatic setting."
						options={destinationOptions}
						disabled={!moveEnabled}
						bind:value={targetRootId}
					/>
				</section>
			{:else if step === 4}
				<section class="space-y-4">
					<div>
						<h3 class="font-display text-lg font-semibold">Review expansion</h3>
						<p class="text-sm text-base-content/55">
							Generating the preview only reads files and saves a plan.
						</p>
					</div>
					<div class="management-review-grid">
						<div><span>Scope</span><strong>{scopeLabel()}</strong></div>
						<div>
							<span>Profile</span><strong
								>{mode === 'baseline_restore' ? 'Original state' : profile?.name}</strong
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
								>Track selection expands to complete releases because organization or sidecar work
								must remain atomic.</span
							>
						</div>{/if}{#if mode === 'baseline_restore'}<div
							class="alert alert-warning items-start"
						>
							<ShieldAlert class="mt-0.5 h-5 w-5" /><span
								>Restore returns files to how they were before DroppedNeedle first managed them. It
								is separate from Undo; restored files stay unmanaged until you enable them again.</span
							>
						</div>{/if}
					<p class="text-sm text-base-content/55">
						Applying is the first write action: the next page lists every file marked eligible,
						warning, blocked, stale, preserved, or unchanged.
					</p>
				</section>
			{/if}
			{#if localError}<div class="alert alert-error mt-4 text-sm" role="alert">
					{localError}
				</div>{/if}
		</div>
	</div>
	{#snippet actions()}
		<div class="flex items-center justify-between gap-2">
			<Button variant="ghost" icon={ArrowLeft} disabled={step === 1 || pending} onclick={back}
				>Back</Button
			>
			<div class="flex gap-2">
				<Button variant="ghost" disabled={pending} onclick={close}>Cancel</Button
				>{#if step < 4}<Button
						variant="warning"
						icon={ArrowRight}
						iconPosition="end"
						disabled={!selectionValid}
						onclick={next}>Continue</Button
					>{:else}<Button
						variant="warning"
						icon={Sparkles}
						loading={pending}
						disabled={!selectionValid || (mode === 'manage' && !profile)}
						onclick={() => void generate()}>Generate preview</Button
					>{/if}
			</div>
		</div>
	{/snippet}
</Modal>

<style>
	.management-review-grid span {
		font-size: 0.7rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: color-mix(in oklab, var(--color-base-content) 48%, transparent);
	}

	.management-runner-progress {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		border-bottom: 1px solid color-mix(in oklab, var(--color-base-content) 9%, transparent);
		background: color-mix(in oklab, var(--color-library-manage) 5%, var(--color-base-100));
	}

	.management-runner-progress > span {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.35rem;
		min-height: 3rem;
		font-family: var(--font-mono);
		font-size: 0.72rem;
		color: color-mix(in oklab, var(--color-base-content) 40%, transparent);
	}

	.management-runner-progress > span[data-state='current'] {
		background: color-mix(in oklab, var(--color-library-manage) 11%, transparent);
		color: var(--color-base-content);
	}

	.management-runner-progress > span[data-state='done'] {
		color: color-mix(in oklab, var(--color-library-manage) 68%, var(--color-base-content));
	}

	.management-runner-progress small {
		font-family: var(--font-sans);
		font-weight: 700;
	}

	.management-scope-tabs {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
	}

	.management-scope-tabs button {
		padding: 0.45rem 0.75rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 12%, transparent);
		border-radius: 9999px;
		font-size: 0.82rem;
	}

	.management-scope-tabs button[aria-pressed='true'] {
		border-color: color-mix(in oklab, var(--color-library-manage) 42%, transparent);
		background: color-mix(in oklab, var(--color-library-manage) 12%, transparent);
		font-weight: 700;
	}

	.management-selected-scope {
		display: grid;
		gap: 0.65rem;
		padding: 0.85rem;
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 24%, transparent);
		border-radius: 0.9rem;
		background: color-mix(in oklab, var(--color-library-manage) 6%, var(--color-base-100));
	}

	.management-selected-scope[data-wide-scope='true'] {
		border-color: color-mix(in oklab, var(--color-warning) 38%, transparent);
		background: color-mix(in oklab, var(--color-warning) 7%, var(--color-base-100));
	}

	.management-selected-scope__header,
	.management-selected-scope__item {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.75rem;
	}

	.management-selected-scope small,
	.management-selected-scope__item span {
		display: block;
	}

	.management-selected-scope small {
		margin-top: 0.1rem;
		color: color-mix(in oklab, var(--color-base-content) 48%, transparent);
	}

	.management-selected-scope__items {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.4rem;
		max-height: 10rem;
		overflow-y: auto;
		overscroll-behavior: contain;
		padding-right: 0.25rem;
		scrollbar-gutter: stable;
	}

	.management-selected-scope__item {
		min-width: 0;
		padding: 0.55rem 0.65rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 9%, transparent);
		border-radius: 0.7rem;
		background: var(--color-base-100);
	}

	.management-selected-scope__item span {
		min-width: 0;
	}

	.management-selected-scope__item strong,
	.management-selected-scope__item small {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.management-runner-profile-list {
		display: grid;
		gap: 0.5rem;
		max-height: min(24rem, 48dvh);
		overflow-y: auto;
		overscroll-behavior: contain;
		padding: 0.15rem 0.35rem 0.15rem 0.15rem;
		scrollbar-gutter: stable;
	}

	.management-runner-profile-list:focus-visible {
		border-radius: 0.25rem;
		outline: 2px solid var(--color-library-manage);
		outline-offset: 0.2rem;
	}

	.management-review-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.75rem;
	}

	.management-review-grid > div {
		display: grid;
		gap: 0.2rem;
		padding: 0.85rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 10%, transparent);
		border-radius: 0.85rem;
	}

	@media (max-width: 639px) {
		.management-runner-progress small {
			display: none;
		}

		.management-review-grid {
			grid-template-columns: 1fr;
		}

		.management-selected-scope__items {
			grid-template-columns: 1fr;
		}
	}
</style>
