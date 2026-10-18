<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { TriangleAlert, ArrowRight, CircleCheckBig, FolderCog, ScanSearch } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import {
		getLibraryRestorableRootsQuery,
		getTargetLibrarySettingsQuery
	} from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import {
		previewLibraryPolicyApply,
		previewLibraryPolicyImpact,
		restoreLibraryRoots,
		saveTargetLibrarySettings
	} from '$lib/queries/library/LibraryPolicyMutations.svelte';
	import { requestLibraryRun } from '$lib/queries/library/LibraryOperationMutations.svelte';
	import { getLibraryStatsQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import {
		getDownloadPolicyQuery,
		saveDownloadPolicy
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import LibraryRootPolicyEditor from '$lib/components/library/LibraryRootPolicyEditor.svelte';
	import LibraryNamingPreview from '$lib/components/library/LibraryNamingPreview.svelte';
	import LibraryScanScheduleControl from '$lib/components/library/LibraryScanScheduleControl.svelte';
	import SettingsLyrics from './SettingsLyrics.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardActions from '$lib/ui/CardActions.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import type {
		LibraryRootSettings,
		TargetLibrarySettingsResponse,
		TypedLibrarySettings
	} from '$lib/queries/library/LibraryOperationsTypes';

	const ACOUSTID_KEY_MASK = 'acoustid****';

	const settingsQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
	const restorableQuery = getLibraryRestorableRootsQuery(() => authStore.isAdmin);
	const impact = previewLibraryPolicyImpact();
	const save = saveTargetLibrarySettings();
	const restore = restoreLibraryRoots();
	const applyPreview = previewLibraryPolicyApply();
	const requestRun = requestLibraryRun();
	const policyQuery = getDownloadPolicyQuery(() => authStore.isAdmin);
	const savePolicy = saveDownloadPolicy();
	const statsQuery = getLibraryStatsQuery();

	let template = $state('');
	let acoustidKey = $state('');
	let roots = $state<LibraryRootSettings[]>([]);
	let libraryEnabled = $state(true);
	let seeded = $state(false);
	let savedSettings = $state<TargetLibrarySettingsResponse | null>(null);
	let impactDialog: HTMLDialogElement;
	let applyDialog: HTMLDialogElement;
	let restoreDialog: HTMLDialogElement;
	let impactHeading: HTMLHeadingElement;
	let applyHeading: HTMLHeadingElement;
	let restoreHeading: HTMLHeadingElement;
	let impactOpener: HTMLElement | null = null;
	let applyOpener: HTMLElement | null = null;
	let restoreOpener: HTMLElement | null = null;
	let restorePaths = $state<Record<string, string>>({});
	let maxLibraryGb = $state<number | null>(0);
	let capSeeded = $state(false);

	const restorableRoots = $derived(restorableQuery.data?.restorable_roots ?? []);

	$effect(() => {
		const data = settingsQuery.data;
		if (data && !seeded) {
			template = data.naming_template;
			acoustidKey = data.acoustid_api_key;
			roots = data.library_roots.map((root) => ({
				...root,
				rules: root.rules.map((rule) => ({ ...rule }))
			}));
			libraryEnabled = data.enabled ?? true;
			seeded = true;
		}
	});

	$effect(() => {
		const policy = policyQuery.data;
		if (policy && !capSeeded) {
			maxLibraryGb = policy.max_library_size_gb;
			capSeeded = true;
		}
	});

	const currentSettings = $derived(savedSettings ?? settingsQuery.data);
	const usedGb = $derived((statsQuery.data?.total_size_bytes ?? 0) / 1024 ** 3);
	const capPercent = $derived(
		maxLibraryGb && maxLibraryGb > 0 ? Math.min(100, (usedGb / maxLibraryGb) * 100) : 0
	);
	const hasKey = $derived(Boolean(acoustidKey));
	const settingsBlockedMessage = $derived(
		!settingsQuery.isLoading && !seeded
			? authStore.isAdmin
				? 'Library settings could not be loaded yet. Reload the page to retry.'
				: 'Sign in as an administrator to change library settings.'
			: null
	);
	const impactErrorMessage = $derived.by((): string | null => {
		if (!impact.isError) return null;
		const error = impact.error;
		if (error instanceof ApiError && error.status > 0 && error.status < 500) {
			return error.message;
		}
		return 'Could not reach the server to preview these settings. Check your connection and try again. Nothing has been saved.';
	});

	function draft(): TypedLibrarySettings {
		return {
			library_roots: roots,
			staging_path: settingsQuery.data?.staging_path ?? '',
			naming_template: template,
			acoustid_api_key: acoustidKey,
			enabled: libraryEnabled
		};
	}

	async function previewSave(event: MouseEvent): Promise<void> {
		if (!seeded) return;
		impactOpener = event.currentTarget as HTMLElement;
		let result;
		try {
			result = await impact.mutateAsync({
				settings: draft(),
				expected_policy_revision: currentSettings?.policy_revision ?? null
			});
		} catch {
			return;
		}
		if (result.stale) {
			toastStore.show({
				message: 'Library settings changed. Reload this page before saving.',
				type: 'error'
			});
			return;
		}
		impactDialog.showModal();
		impactHeading.focus();
	}

	async function confirmSave(): Promise<void> {
		const expectedRevision = impact.data?.current_policy_revision;
		if (!expectedRevision) return;
		const result = await save.mutateAsync({
			settings: draft(),
			expected_policy_revision: expectedRevision
		});
		savedSettings = result;
		impactDialog.close();
	}

	function openRestore(event: MouseEvent): void {
		restoreOpener = event.currentTarget as HTMLElement;
		restorePaths = Object.fromEntries(restorableRoots.map((entry) => [entry.root_id, entry.path]));
		restoreDialog.showModal();
		restoreHeading.focus();
	}

	async function confirmRestore(): Promise<void> {
		const revision = restorableQuery.data?.policy_revision;
		if (!revision) return;
		try {
			const result = await restore.mutateAsync({
				expected_policy_revision: revision,
				paths: restorePaths
			});
			const draftIds = new Set(roots.map((root) => root.id));
			roots = [
				...roots,
				...result.library_roots
					.filter((root) => !draftIds.has(root.id))
					.map((root) => ({
						...root,
						rules: root.rules.map((rule) => ({ ...rule }))
					}))
			];
			restoreDialog.close();
		} catch {
			return;
		}
	}

	async function previewApply(event: MouseEvent): Promise<void> {
		applyOpener = event.currentTarget as HTMLElement;
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
		applyDialog.showModal();
		applyHeading.focus();
	}

	async function confirmApply(): Promise<void> {
		const preview = applyPreview.data;
		if (!preview) return;
		await requestRun.mutateAsync({
			kind: 'policy_reconcile',
			scope_ids: preview.scope_ids,
			expected_policy_revision: preview.policy_revision
		});
		applyDialog.close();
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

	async function handleSaveCap(): Promise<void> {
		const policy = policyQuery.data;
		if (!policy) return;
		try {
			await savePolicy.mutateAsync({ ...policy, max_library_size_gb: maxLibraryGb ?? 0 });
			toastStore.show({ message: 'Storage cap saved', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save the storage cap', type: 'error' });
		}
	}
</script>

<div class="space-y-6">
	<div>
		<h2 class="text-xl font-bold">Library</h2>
		<p class="text-sm text-base-content/60">
			Choose how DroppedNeedle observes your library and, separately, whether it may change it.
		</p>
	</div>

	{#if settingsQuery.isLoading}
		<div class="space-y-3">
			<div class="skeleton h-48 rounded-box"></div>
			<div class="skeleton h-28 rounded-box"></div>
		</div>
	{:else if settingsQuery.isError && !settingsQuery.data}
		<div class="alert alert-error">
			<span
				>Could not load library settings.{#if settingsQuery.error?.message}
					{settingsQuery.error.message}{/if}</span
			>
		</div>
	{:else}
		{#if settingsQuery.isError}
			<div class="alert alert-warning text-sm">
				<TriangleAlert class="h-4 w-4" /><span
					>Could not refresh library settings. You are seeing the last loaded values; saving still
					checks for conflicting changes.</span
				>
			</div>
		{/if}
		<header class="library-scanning-header">
			<div class="library-scanning-mark"><ScanSearch class="h-6 w-6" /></div>
			<div>
				<p class="library-scanning-kicker">Read-only catalog work</p>
				<h2 class="font-display text-xl font-semibold">Scanning &amp; identification</h2>
				<p class="mt-1 text-sm text-base-content/65">
					Reads files and updates DroppedNeedle. It does not change your music files.
				</p>
			</div>
		</header>

		{#if !hasKey}
			<div class="alert alert-warning">
				<TriangleAlert class="h-5 w-5" /><span class="text-sm"
					>No AcoustID key - fingerprint identification is off for files without MusicBrainz tags.
					Add a key to enable it.</span
				>
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
					onclick={(event: MouseEvent) => void previewApply(event)}>Apply changes...</Button
				>
			</div>
		{/if}
		{#if applyPreview.isError}
			<div class="alert alert-error text-sm">
				Could not preview policy reconciliation. No scan was started.
			</div>
		{/if}

		{#if restorableRoots.length > 0}
			<div class="alert alert-warning items-start">
				<TriangleAlert class="mt-0.5 h-5 w-5" />
				<div class="min-w-0 flex-1">
					<strong>Library roots were removed</strong>
					<p class="text-sm">
						This page is missing {restorableRoots.length} library
						{restorableRoots.length === 1 ? 'root' : 'roots'} that the catalog still uses. Restore
						{restorableRoots.length === 1 ? 'it' : 'them'} to keep your library working without a rescan.
					</p>
				</div>
				<Button
					variant="warning"
					size="sm"
					loading={restore.isPending}
					onclick={(event: MouseEvent) => openRestore(event)}>Restore roots...</Button
				>
			</div>
		{/if}

		<Card as="section" variant="bordered">
			<div class="flex flex-col gap-6">
				<ToggleRow
					label="Local library"
					description="Scanning, identification, and file organization run while this is on. Turn it off to pause the library without removing your roots. Existing catalog data and playback keep working."
					bind:checked={libraryEnabled}
				/>

				<div class="divider my-0"></div>

				<LibraryRootPolicyEditor {roots} onchange={(value) => (roots = value)} />

				<div class="divider my-0"></div>

				<section class="space-y-2">
					<h3 class="font-semibold">Legacy import naming template</h3>
					<p class="text-xs text-base-content/60">
						Fallback for downloaded imports that file organization does not handle. Once
						organization is enabled, its assigned profile controls managed paths. Variables:
						{'{initial} {albumartist} {album} {year} {disc} {track} {title} {ext}'}.
						{'{initial}'} uses the effective album artist, ignores leading Unicode whitespace and a case-insensitive
						"The" plus following Unicode whitespace, then returns one uppercase letter; empty or nonletter
						leads return "#".
					</p>
					<Input label="Naming template" mono class="w-full" bind:value={template} />
					<LibraryNamingPreview {template} />
				</section>

				<div class="divider my-0"></div>

				<SecretField
					label="AcoustID API key"
					mask={ACOUSTID_KEY_MASK}
					placeholder="Enter AcoustID API key"
					help="Used to identify files that do not contain MusicBrainz tags."
					bind:value={acoustidKey}
				/>

				{#if settingsBlockedMessage}
					<p class="text-sm text-warning">{settingsBlockedMessage}</p>
				{/if}
				<CardActions
					class="items-center justify-between gap-3 border-t border-base-content/10 pt-5"
				>
					{#if impactErrorMessage}
						<p class="text-sm text-error">{impactErrorMessage}</p>
					{:else}
						<span></span>
					{/if}
					<Button
						variant="primary"
						loading={impact.isPending || save.isPending}
						disabled={!seeded}
						onclick={(event: MouseEvent) => void previewSave(event)}
						>Preview and save settings</Button
					>
				</CardActions>
			</div>
		</Card>

		<Card as="section" variant="bordered">
			<div class="flex flex-col gap-4">
				<div>
					<h3 class="font-semibold">Storage cap</h3>
					<p class="mt-1 text-xs text-base-content/60">
						Block new downloads once the library reaches this size. 0 = unlimited. Nothing is
						deleted, and quality upgrades are exempt because they replace existing files.
					</p>
				</div>
				<div class="flex flex-wrap items-end gap-3">
					<Field label="Maximum library size (GB)">
						{#snippet children(control)}
							<Input
								{...control}
								label="Maximum library size (GB)"
								type="number"
								size="sm"
								min="0"
								max="1000000"
								class="w-40"
								bind:value={maxLibraryGb}
							/>
						{/snippet}
					</Field>
					<Button
						variant="primary"
						size="sm"
						class="mb-1"
						loading={savePolicy.isPending}
						disabled={!capSeeded}
						onclick={() => void handleSaveCap()}>Save cap</Button
					>
				</div>
				{#if maxLibraryGb && maxLibraryGb > 0}<div class="max-w-md space-y-1">
						<progress
							class="progress w-full {capPercent >= 100
								? 'progress-error'
								: capPercent >= 85
									? 'progress-warning'
									: 'progress-primary'}"
							value={capPercent}
							max="100"
						></progress>
						<p class="text-xs text-base-content/60">
							{usedGb.toFixed(1)} GB of {maxLibraryGb} GB used{#if capPercent >= 100}
								- new downloads are blocked{/if}
						</p>
					</div>{:else}<p class="text-xs text-base-content/40">
						Library size: {usedGb.toFixed(1)} GB (no cap set)
					</p>{/if}
			</div>
		</Card>

		<Card as="section" variant="bordered">
			<div class="flex flex-col gap-5">
				<section class="space-y-2">
					<h3 class="font-semibold">Automatic scanning</h3>
					<p class="text-xs text-base-content/60">
						How often DroppedNeedle scans your library for new and changed files.
					</p>
					<LibraryScanScheduleControl />
				</section>
				<div class="divider my-0"></div>
				<section class="space-y-2">
					<h3 class="font-semibold">Manual scan</h3>
					<Button
						variant="outline"
						size="sm"
						loading={requestRun.isPending}
						disabled={roots.length === 0 || !libraryEnabled}
						onclick={() => void scanForChanges()}>Scan for changes</Button
					>
					{#if !libraryEnabled}<p class="text-xs text-warning">
							The local library is disabled. Re-enable it before starting a scan.
						</p>{/if}
				</section>
			</div>
		</Card>

		{#if authStore.isAdmin}
			<SettingsLyrics />
			<section class="management-settings-portal">
				<div class="management-write-mark"><FolderCog class="h-6 w-6" /></div>
				<div class="min-w-0 flex-1">
					<p class="management-kicker">Administrator workspace</p>
					<h3 class="font-display text-lg font-semibold">Organize files</h3>
					<p class="mt-1 text-sm text-base-content/60">
						Profiles, automatic write access, dry runs, and recovery live in the Library Management
						workspace under the Automation tab.
					</p>
				</div>
				<Button
					variant="warning"
					size="sm"
					href={withBasePath('/library/management?tab=automation')}
					icon={ArrowRight}
					iconPosition="end">Open Organize files settings</Button
				>
			</section>
		{/if}
	{/if}
</div>

<dialog
	bind:this={impactDialog}
	class="modal"
	aria-labelledby="policy-impact-title"
	onclose={() => impactOpener?.focus()}
>
	<div class="modal-box max-w-lg">
		<h2 bind:this={impactHeading} id="policy-impact-title" tabindex="-1" class="text-lg font-bold">
			Save library policy changes?
		</h2>
		{#if impact.data}<div class="mt-4 space-y-3 text-sm">
				<p>
					<strong
						>{impact.data.indexed_file_count?.toLocaleString() ?? 'An unknown number of'} indexed files</strong
					>
					and {impact.data.on_disk_file_count?.toLocaleString() ?? 'an unknown number of'} files on disk
					are in the affected scopes.
				</p>
				{#if impact.data.content_will_become_unavailable}<div class="alert alert-warning">
						<TriangleAlert class="h-4 w-4" /> Some music will become unavailable after you explicitly
						apply reconciliation.
					</div>{/if}{#if impact.data.queued_work_will_be_cancelled}<p class="text-warning">
						Queued work in these scopes will be cancelled when the policy is saved.
					</p>{/if}{#each impact.data.warnings as warning, index (`${index}:${warning}`)}<p
						class="text-base-content/60"
					>
						{warning}
					</p>{/each}
				<p class="flex items-center gap-2 text-success">
					<CircleCheckBig class="h-4 w-4" /> Saving does not start a scan.
				</p>
			</div>{/if}
		{#if save.error}
			<p class="mt-3 text-sm text-error">{save.error.message}</p>
		{/if}
		<div class="modal-action">
			<Button variant="ghost" onclick={() => impactDialog.close()}>Cancel</Button><Button
				variant="primary"
				loading={save.isPending}
				onclick={() => void confirmSave()}>Save policies</Button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close policy impact dialog">close</button>
	</form>
</dialog>

<dialog
	bind:this={applyDialog}
	class="modal"
	aria-labelledby="policy-apply-title"
	onclose={() => applyOpener?.focus()}
>
	<div class="modal-box max-w-lg">
		<h2 bind:this={applyHeading} id="policy-apply-title" tabindex="-1" class="text-lg font-bold">
			Apply policy changes?
		</h2>
		{#if applyPreview.data}<p class="mt-3 text-sm text-base-content/70">
				About {applyPreview.data.estimated_file_count.toLocaleString()} files will be reconciled. Existing
				committed catalog changes remain safe if the job is paused or stopped.
			</p>
			{#if applyPreview.data.content_will_become_unavailable}<div class="alert alert-warning mt-3">
					<TriangleAlert class="h-4 w-4" /> Music under Excluded scopes will become unavailable to DroppedNeedle
					and connected clients.
				</div>{/if}{/if}
		<div class="modal-action">
			<Button variant="ghost" onclick={() => applyDialog.close()}>Cancel</Button><Button
				variant="warning"
				loading={requestRun.isPending}
				onclick={() => void confirmApply()}>Apply policy changes</Button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close apply policy dialog">close</button>
	</form>
</dialog>

<dialog
	bind:this={restoreDialog}
	class="modal"
	aria-labelledby="restore-roots-title"
	onclose={() => restoreOpener?.focus()}
>
	<div class="modal-box max-w-xl">
		<h2 bind:this={restoreHeading} id="restore-roots-title" tabindex="-1" class="text-lg font-bold">
			Restore removed library roots
		</h2>
		<p class="mt-2 text-sm text-base-content/60">
			These roots still have files in the catalog but were dropped from the configuration. Restoring
			keeps their original identities, so nothing is scanned twice.
		</p>
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
		<div class="modal-action">
			<Button variant="ghost" onclick={() => restoreDialog.close()}>Cancel</Button><Button
				variant="warning"
				loading={restore.isPending}
				onclick={() => void confirmRestore()}
				>Restore {restorableRoots.length === 1 ? 'root' : 'roots'}</Button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close restore roots dialog">close</button>
	</form>
</dialog>

<style>
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
