<script lang="ts">
	import SettingsPaneHeader from './SettingsPaneHeader.svelte';
	import '$lib/features/library-management/components/library-management.css';
	import { TriangleAlert, ArrowRight, FolderCog, ScanSearch } from 'lucide-svelte';
	import {
		getLibraryRestorableRootsQuery,
		getRemovedLibraryRootsQuery
	} from '$lib/features/library-management/queries/LibraryPolicyQueries.svelte';
	import {
		createDropRemovedRootTracksMutation,
		createPreviewLibraryPolicyApplyMutation,
		createRestoreLibraryRootsMutation
	} from '$lib/features/library-management/queries/LibraryPolicyMutations.svelte';
	import { createRequestLibraryRunMutation } from '$lib/features/library-management/queries/LibraryOperationMutations.svelte';
	import {
		createSaveLibraryWatcherSettingsMutation,
		getLibraryWatcherSettingsQuery
	} from '$lib/features/settings/queries/SectionSettingsQueries.svelte';
	import { getLibraryStatsQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import {
		getDownloadPolicyQuery,
		createSaveDownloadPolicyMutation
	} from '$lib/features/downloads/queries/DownloadClientsQueries.svelte';
	import LibraryScanScheduleControl from '$lib/features/library-management/components/LibraryScanScheduleControl.svelte';
	import SettingsImportFolder from './SettingsImportFolder.svelte';
	import SettingsSoundAnalysis from './SettingsSoundAnalysis.svelte';
	import SettingsLibraryCard from './SettingsLibraryCard.svelte';
	import { createLibraryCard } from './libraryCard.svelte';
	import SettingsLyrics from './SettingsLyrics.svelte';
	import SettingsCovers from './SettingsCovers.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	const library = createLibraryCard();
	const { settingsQuery, section } = library;
	const restorableQuery = getLibraryRestorableRootsQuery(() => authStore.isAdmin);
	const removedRootsQuery = getRemovedLibraryRootsQuery(() => authStore.isAdmin);
	const restore = createRestoreLibraryRootsMutation();
	const dropRemovedTracks = createDropRemovedRootTracksMutation();
	const applyPreview = createPreviewLibraryPolicyApplyMutation();
	const requestRun = createRequestLibraryRunMutation();
	const policyQuery = getDownloadPolicyQuery(() => authStore.isAdmin);
	const savePolicy = createSaveDownloadPolicyMutation();
	const statsQuery = getLibraryStatsQuery();
	// Only the switch is shown; the poll and batch intervals keep their values.
	const watcher = createSettingsSection(
		getLibraryWatcherSettingsQuery(() => authStore.isAdmin),
		createSaveLibraryWatcherSettingsMutation()
	);

	let applyOpen = $state(false);
	let restoreOpen = $state(false);
	let restorePaths = $state<Record<string, string>>({});
	let dropOpen = $state(false);
	let dropTarget = $state<UnifiedRemovedRoot | null>(null);

	interface UnifiedRemovedRoot {
		root_id: string;
		path: string | null;
		indexed_track_count: number;
		restorable: boolean;
		overlapping_root_label: string | null;
	}

	// The storage cap is one field of the download policy.
	const cap = createSettingsSection<{ max_library_size_gb: number | null }>(
		{
			get data() {
				const policy = policyQuery.data;
				return policy ? { max_library_size_gb: policy.max_library_size_gb } : undefined;
			},
			get isPending() {
				return policyQuery.isPending;
			},
			get isError() {
				return policyQuery.isError;
			},
			get error() {
				return policyQuery.error;
			},
			refetch: () => policyQuery.refetch()
		},
		{
			mutateAsync: async (form) => {
				const policy = policyQuery.data;
				if (!policy) throw new Error('The download policy has not loaded.');
				const saved = await savePolicy.mutateAsync({
					...policy,
					max_library_size_gb: form.max_library_size_gb ?? 0
				});
				return { max_library_size_gb: saved.max_library_size_gb };
			},
			get isPending() {
				return savePolicy.isPending;
			}
		},
		{ normalize: (form) => ({ max_library_size_gb: form.max_library_size_gb ?? 0 }) }
	);

	const restorableRoots = $derived(restorableQuery.data?.restorable_roots ?? []);
	const removedRoots = $derived(removedRootsQuery.data?.removed_roots ?? []);
	const currentSettings = $derived(library.currentSettings);
	const removedRootsRevision = $derived(
		removedRootsQuery.data?.policy_revision || currentSettings?.policy_revision || ''
	);
	const unifiedRemovedRoots = $derived.by((): UnifiedRemovedRoot[] => {
		const byId: Record<string, UnifiedRemovedRoot> = Object.create(null);
		for (const root of removedRoots) {
			byId[root.root_id] = {
				root_id: root.root_id,
				path: root.path,
				indexed_track_count: root.indexed_track_count,
				restorable: root.restorable,
				overlapping_root_label: root.overlapping_root_label
			};
		}
		for (const root of restorableRoots) {
			const existing = byId[root.root_id];
			if (existing) {
				existing.restorable = true;
				if (!existing.path && root.path) existing.path = root.path;
			} else {
				byId[root.root_id] = {
					root_id: root.root_id,
					path: root.path,
					indexed_track_count: 0,
					restorable: true,
					overlapping_root_label: null
				};
			}
		}
		return Object.values(byId).sort((a, b) => a.root_id.localeCompare(b.root_id));
	});
	const roots = $derived(section.draft?.library_roots ?? []);
	const libraryEnabled = $derived(section.draft?.enabled ?? true);
	const usedGb = $derived((statsQuery.data?.total_size_bytes ?? 0) / 1024 ** 3);
	const maxLibraryGb = $derived(cap.draft?.max_library_size_gb ?? 0);
	const capPercent = $derived(maxLibraryGb > 0 ? Math.min(100, (usedGb / maxLibraryGb) * 100) : 0);

	function openRestore(): void {
		restorePaths = Object.fromEntries(restorableRoots.map((entry) => [entry.root_id, entry.path]));
		restoreOpen = true;
	}

	function openDropRemoved(entry: UnifiedRemovedRoot): void {
		dropTarget = entry;
		dropOpen = true;
	}

	async function confirmDropRemoved(): Promise<void> {
		if (!dropTarget || !removedRootsRevision) return;
		await dropRemovedTracks.mutateAsync({
			root_id: dropTarget.root_id,
			expected_policy_revision: removedRootsRevision
		});
	}

	function removedRootStatus(entry: UnifiedRemovedRoot): string {
		const countText =
			entry.indexed_track_count > 0
				? `${entry.indexed_track_count.toLocaleString()} indexed ${entry.indexed_track_count === 1 ? 'track' : 'tracks'}`
				: 'No indexed tracks';
		if (entry.overlapping_root_label) {
			return `${countText} · cannot restore (path overlaps configured root)`;
		}
		if (entry.restorable) {
			return `${countText} · can be restored without rescanning`;
		}
		return countText;
	}

	async function confirmRestore(): Promise<void> {
		const revision = restorableQuery.data?.policy_revision;
		const draft = section.draft;
		if (!revision || !draft) return;
		const result = await restore.mutateAsync({
			expected_policy_revision: revision,
			paths: restorePaths
		});
		const draftIds = new Set(draft.library_roots.map((root) => root.id));
		draft.library_roots = [
			...draft.library_roots,
			...result.library_roots
				.filter((root) => !draftIds.has(root.id))
				.map((root) => ({
					...root,
					rules: root.rules.map((rule) => ({ ...rule }))
				}))
		];
	}

	async function previewApply(): Promise<void> {
		const settings = currentSettings;
		if (!settings?.pending_policy_revision) return;
		try {
			await applyPreview.mutateAsync({
				scope_ids: settings.affected_scope_ids,
				expected_policy_revision: settings.pending_policy_revision
			});
		} catch {
			return;
		}
		applyOpen = true;
	}

	async function confirmApply(): Promise<void> {
		const preview = applyPreview.data;
		if (!preview) return;
		await requestRun.mutateAsync({
			kind: 'policy_reconcile',
			scope_ids: preview.scope_ids,
			expected_policy_revision: preview.policy_revision
		});
	}

	async function scanForChanges(): Promise<void> {
		const revision = currentSettings?.policy_revision;
		if (!revision) return;
		await requestRun.mutateAsync({
			kind: 'incremental',
			scope_ids: [],
			expected_policy_revision: revision
		});
	}
</script>

<div class="space-y-6">
	<SettingsPaneHeader
		title="Library"
		description="Choose how DroppedNeedle observes your library and, separately, whether it may change it."
	/>

	{#if settingsQuery.isError && settingsQuery.data}
		<div class="alert alert-warning text-sm">
			<TriangleAlert class="h-4 w-4" /><span>
				Could not refresh library settings. You are seeing the last loaded values; saving still
				checks for conflicting changes.
			</span>
		</div>
	{/if}
	<header class="library-scanning-header">
		<div class="library-scanning-mark"><ScanSearch class="h-6 w-6" /></div>
		<div>
			<p class="library-scanning-kicker">Read-only catalog work</p>
			<h2 class="font-display text-xl font-semibold">Scanning &amp; identification</h2>
			<p class="mt-1 text-sm text-muted">
				Reads files and updates DroppedNeedle. It does not change your music files.
			</p>
		</div>
	</header>

	{#if section.draft && !section.draft.acoustid_api_key}
		<div class="alert alert-warning">
			<TriangleAlert class="h-5 w-5" /><span class="text-sm">
				No AcoustID key - fingerprint identification is off for files without MusicBrainz tags. Add
				a key to enable it.
			</span>
		</div>
	{/if}

	{#if currentSettings?.reconciliation_required}
		<div class="alert alert-warning items-start">
			<TriangleAlert class="mt-0.5 h-5 w-5" />
			<div class="min-w-0 flex-1">
				<strong>Awaiting reconciliation</strong>
				<p class="text-sm">
					The saved policies already prevent prohibited new work. Apply the changes when you are
					ready to update existing catalog availability.
				</p>
			</div>
			<Button
				variant="warning"
				size="sm"
				loading={applyPreview.isPending}
				onclick={() => void previewApply()}
			>
				Apply changes...
			</Button>
		</div>
	{/if}
	{#if applyPreview.isError}
		<div class="alert alert-error text-sm">
			Could not preview policy reconciliation. No scan was started.
		</div>
	{/if}

	{#if unifiedRemovedRoots.length > 0}
		<section class="removed-roots-banner" aria-label="Removed library folders still in catalog">
			<div class="flex flex-wrap items-start justify-between gap-3">
				<div class="min-w-0 flex-1">
					<div class="flex items-center gap-2 text-sm font-semibold text-warning">
						<TriangleAlert class="h-4 w-4 shrink-0" />
						<span>Removed library folders still in catalog</span>
					</div>
					<p class="mt-1 text-sm text-muted">
						These folders are no longer configured in Settings, but the catalog still holds tracks
						for them. Restore a folder to keep using it, or drop its tracks from the catalog.
					</p>
				</div>
				{#if restorableRoots.length > 0}
					<Button variant="outline" size="sm" loading={restore.isPending} onclick={openRestore}>
						Restore roots...
					</Button>
				{/if}
			</div>
			<div class="mt-3 space-y-2">
				{#each unifiedRemovedRoots as entry (entry.root_id)}
					<div class="removed-roots-row">
						<div class="min-w-0 flex-1 space-y-0.5">
							<div class="flex flex-wrap items-center gap-2">
								<strong class="break-all font-mono text-sm">{entry.path ?? entry.root_id}</strong>
								{#if entry.overlapping_root_label}
									<span class="removed-roots-tag removed-roots-tag-warn">
										Overlaps "{entry.overlapping_root_label}"
									</span>
								{:else if entry.restorable}
									<span class="removed-roots-tag">Folder on disk</span>
								{/if}
							</div>
							<p class="text-xs text-muted">{removedRootStatus(entry)}</p>
						</div>
						<div class="flex flex-wrap items-center gap-2">
							{#if entry.indexed_track_count > 0}
								<Button
									variant="warning"
									size="sm"
									disabled={!removedRootsRevision || dropRemovedTracks.isPending}
									onclick={() => openDropRemoved(entry)}
								>
									Drop tracks...
								</Button>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		</section>
	{/if}

	<SettingsLibraryCard card={library} />

	{#if authStore.isAdmin}
		<SettingsImportFolder aboveReady={!settingsQuery.isPending} />
		<SettingsSoundAnalysis />
	{/if}

	<Card as="section" variant="bordered">
		<div class="flex flex-col gap-4">
			<div>
				<h3 class="font-semibold">Storage cap</h3>
				<p class="mt-1 text-xs text-muted">
					Block new downloads once the library reaches this size. 0 = unlimited. Nothing is deleted,
					and quality upgrades are exempt because they replace existing files.
				</p>
			</div>
			<QueryBoundary query={cap} what="the storage cap">
				{#snippet children(draft)}
					<div class="flex flex-col gap-3">
						<div class="max-w-xs">
							<NumberField
								label="Maximum library size (GB)"
								size="sm"
								min={0}
								max={1000000}
								bind:value={
									() => draft.max_library_size_gb ?? 0, (next) => (draft.max_library_size_gb = next)
								}
								error={cap.fieldError('max_library_size_gb', 'Maximum library size')}
							/>
						</div>
						<SaveBar {...cap.bar} saveLabel="Save cap" />
					</div>
				{/snippet}
			</QueryBoundary>
			{#if maxLibraryGb > 0}<div class="max-w-md space-y-1">
					<progress
						class="progress w-full {capPercent >= 100
							? 'progress-error'
							: capPercent >= 85
								? 'progress-warning'
								: 'progress-primary'}"
						value={capPercent}
						max="100"
					></progress>
					<p class="text-xs text-muted">
						{usedGb.toFixed(1)} GB of {maxLibraryGb} GB used{#if capPercent >= 100}
							- new downloads are blocked{/if}
					</p>
				</div>{:else}<p class="text-xs text-muted">
					Library size: {usedGb.toFixed(1)} GB (no cap set)
				</p>{/if}
		</div>
	</Card>

	<Card as="section" variant="bordered">
		<div class="flex flex-col gap-5">
			<section class="space-y-2">
				<h3 class="font-semibold">Automatic scanning</h3>
				<p class="text-xs text-muted">
					How often DroppedNeedle scans your library for new and changed files.
				</p>
				<LibraryScanScheduleControl />
				{#if authStore.isAdmin}
					<QueryBoundary query={watcher} what="the folder watcher">
						{#snippet children(draft)}
							<div class="space-y-3 pt-2">
								<ToggleRow
									label="Watch folders for changes"
									description="Picks up new and changed files between scheduled scans."
									bind:checked={draft.enabled}
								/>
								<SaveBar {...watcher.bar} />
							</div>
						{/snippet}
					</QueryBoundary>
				{/if}
			</section>
			<div class="divider my-0"></div>
			<section class="space-y-2">
				<h3 class="font-semibold">Manual scan</h3>
				<Button
					variant="outline"
					size="sm"
					loading={requestRun.isPending}
					disabled={roots.length === 0 || !libraryEnabled}
					onclick={() => void scanForChanges()}
				>
					Scan for changes
				</Button>
				{#if !libraryEnabled}<p class="text-xs text-warning">
						The local library is disabled. Re-enable it before starting a scan.
					</p>{/if}
			</section>
		</div>
	</Card>

	{#if authStore.isAdmin}
		<SettingsLyrics />
		<SettingsCovers />
		<section class="management-settings-portal">
			<div class="management-write-mark"><FolderCog class="h-6 w-6" /></div>
			<div class="min-w-0 flex-1">
				<p class="management-kicker">Administrator workspace</p>
				<h3 class="font-display text-lg font-semibold">Organize files</h3>
				<p class="mt-1 text-sm text-muted">
					Profiles, automatic write access, dry runs, and recovery live in the Library Management
					workspace under the Automation tab.
				</p>
			</div>
			<Button
				variant="warning"
				size="sm"
				href={withBasePath('/library/management?tab=automation')}
				icon={ArrowRight}
				iconPosition="end"
			>
				Open Organize files settings
			</Button>
		</section>
	{/if}
</div>

<ConfirmDialog
	bind:open={applyOpen}
	title="Apply policy changes?"
	confirmLabel="Apply policy changes"
	tone="warning"
	focusTitle
	pending={requestRun.isPending}
	onconfirm={confirmApply}
>
	{#if applyPreview.data}<p class="text-sm text-muted">
			About {applyPreview.data.estimated_file_count.toLocaleString()} files will be reconciled. Existing
			committed catalog changes remain safe if the job is paused or stopped.
		</p>
		{#if applyPreview.data.content_will_become_unavailable}<div class="alert alert-warning mt-3">
				<TriangleAlert class="h-4 w-4" /> Music under Excluded scopes will become unavailable to DroppedNeedle
				and connected clients.
			</div>{/if}{/if}
</ConfirmDialog>

<ConfirmDialog
	bind:open={restoreOpen}
	title="Restore removed library roots"
	message="These roots still have files in the catalog but were dropped from the configuration. Restoring keeps their original identities, so nothing is scanned twice."
	confirmLabel="Restore {restorableRoots.length === 1 ? 'root' : 'roots'}"
	tone="warning"
	focusTitle
	pending={restore.isPending}
	onconfirm={confirmRestore}
>
	<div class="mt-4 space-y-4">
		{#each restorableRoots as entry (entry.root_id)}
			<Field
				label={`Root ${entry.root_id.slice(0, 8)}…`}
				help={`${
					entry.indexed_file_count > 0
						? `Recovered from ${entry.indexed_file_count.toLocaleString()} catalog ${entry.indexed_file_count === 1 ? 'file' : 'files'}.`
						: 'Check this is the original root path.'
				} A wrong path duplicates your library on the next scan.`}
			>
				{#snippet children(control)}
					<Input
						{...control}
						label={`Path for ${entry.root_id}`}
						mono
						class="w-full"
						bind:value={restorePaths[entry.root_id]}
					/>
				{/snippet}
			</Field>
		{/each}
	</div>
</ConfirmDialog>

<ConfirmDialog
	bind:open={dropOpen}
	title={dropTarget
		? `Drop catalog tracks for ${dropTarget.path ?? dropTarget.root_id}?`
		: 'Drop catalog tracks?'}
	confirmLabel="Drop catalog tracks"
	cancelLabel="Cancel"
	tone="warning"
	focusTitle
	pending={dropRemovedTracks.isPending}
	canConfirm={Boolean(dropTarget && removedRootsRevision)}
	onconfirm={confirmDropRemoved}
>
	{#if dropTarget}
		<p class="text-sm text-muted">
			This marks <strong class="text-base-content">
				{dropTarget.indexed_track_count.toLocaleString()} indexed
				{dropTarget.indexed_track_count === 1 ? 'track' : 'tracks'}
			</strong>
			from the removed folder
			<span class="break-all font-mono text-base-content">
				{dropTarget.path ?? dropTarget.root_id}
			</span>
			as missing in the catalog.{#if dropTarget.overlapping_root_label}
				Your active root "{dropTarget.overlapping_root_label}" is not affected.{/if}
		</p>
		<p class="mt-2 text-xs text-muted">
			No files on disk are deleted. Play history, favorites and ratings stay in the database.
		</p>
	{/if}
</ConfirmDialog>

<style>
	.removed-roots-banner {
		padding: 1rem 1.25rem;
		border: 1.5px solid color-mix(in oklab, var(--color-warning) 45%, transparent);
		border-radius: var(--radius-box);
		background-color: color-mix(in oklab, var(--color-warning) 10%, var(--color-base-200));
	}
	.removed-roots-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.75rem 0.875rem;
		border: 1px solid color-mix(in oklab, var(--color-warning) 28%, transparent);
		border-radius: var(--radius-field);
		background-color: color-mix(in oklab, var(--color-base-100) 75%, transparent);
	}
	.removed-roots-tag {
		display: inline-block;
		padding: 0.125rem 0.45rem;
		border: 1px solid var(--dn-edge);
		border-radius: 0.25rem;
		background-color: var(--color-base-300);
		color: var(--dn-muted);
		font-family: var(--font-mono);
		font-size: 0.625rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
	.removed-roots-tag-warn {
		border-color: color-mix(in oklab, var(--color-warning) 35%, transparent);
		background-color: color-mix(in oklab, var(--color-warning) 15%, transparent);
		color: var(--color-warning);
	}
	.library-scanning-header {
		display: flex;
		align-items: flex-start;
		gap: 1rem;
		padding: 1.25rem;
		border: 1px solid color-mix(in oklab, var(--color-library-index) 22%, transparent);
		border-radius: var(--radius-box);
		background:
			linear-gradient(
				135deg,
				color-mix(in oklab, var(--color-library-index) 9%, var(--color-base-100)),
				color-mix(in oklab, var(--color-library-identify) 5%, var(--color-base-100))
			),
			var(--color-base-100);
	}

	.library-scanning-mark {
		display: grid;
		width: 2.75rem;
		height: 2.75rem;
		flex: none;
		place-items: center;
		border-radius: 0.9rem;
	}

	.library-scanning-mark {
		background: color-mix(in oklab, var(--color-library-index) 14%, var(--color-base-100));
		color: var(--color-library-index);
	}

	.management-settings-portal {
		display: flex;
		align-items: center;
		gap: 1rem;
		padding: 1.25rem;
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 28%, transparent);
		border-radius: var(--radius-box);
		background:
			linear-gradient(
				135deg,
				color-mix(in oklab, var(--color-library-manage) 8%, var(--color-base-100)),
				var(--color-base-100)
			),
			var(--color-base-100);
	}

	@media (max-width: 639px) {
		.management-settings-portal {
			align-items: flex-start;
			flex-wrap: wrap;
		}

		.management-settings-portal > :global(.btn) {
			width: 100%;
		}
	}
</style>
