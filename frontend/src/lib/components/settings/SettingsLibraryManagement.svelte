<!--
	The Automation tab of Library Management: profiles, where writes are
	allowed, retention, and the save bar. The profile list, the root cards and
	the profile editor edit one settings-section draft. Saving checks the
	change first; a change that widens what automatic work writes goes
	through the activation wizard's dry runs instead of saving straight away.
-->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { onMount, untrack } from 'svelte';
	import {
		Check,
		ChevronRight,
		CircleOff,
		FolderCog,
		History,
		RefreshCw,
		ShieldAlert
	} from 'lucide-svelte';

	import LibraryManagementActivationDialog from './LibraryManagementActivationDialog.svelte';
	import LibraryManagementProfileEditor from './LibraryManagementProfileEditor.svelte';
	import LibraryManagementProfileList from './LibraryManagementProfileList.svelte';
	import LibraryManagementProfileSharing from './LibraryManagementProfileSharing.svelte';
	import LibraryManagementPurgeDialog from './LibraryManagementPurgeDialog.svelte';
	import LibraryManagementRootAssignment from './LibraryManagementRootAssignment.svelte';
	import { ActivationWizard } from './activationWizard.svelte';
	import { createManagementSettings, writesAutomatically } from './managementSettingsDraft.svelte';
	import { ApiError } from '$lib/api/errors';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { LibraryRootSettings } from '$lib/queries/library/LibraryOperationsTypes';
	import {
		getLibraryManagementActivationHealthQuery,
		getLibraryManagementOperationsQuery
	} from '$lib/queries/library-management/LibraryManagementQueries.svelte';
	import {
		previewLibraryManagementSettingsImpactMutation,
		validateLibraryManagementSettingsMutation
	} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import type {
		LibraryManagementProfile,
		LibraryManagementProfileImportResponse,
		ManagementScriptSettings
	} from '$lib/queries/library-management/types';
	import Button from '$lib/ui/Button.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import UnsavedChangesGuard from '$lib/ui/UnsavedChangesGuard.svelte';

	interface Props {
		roots: LibraryRootSettings[];
		policyRevision: string;
	}

	let { roots, policyRevision }: Props = $props();
	const settings = createManagementSettings();
	const section = settings.section;
	const operationsQuery = getLibraryManagementOperationsQuery(
		() => authStore.user?.id,
		() => ({ limit: 20 })
	);
	const activationHealthQuery = getLibraryManagementActivationHealthQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const validateSettings = validateLibraryManagementSettingsMutation();
	const impactSettings = previewLibraryManagementSettingsImpactMutation();

	const wizard = new ActivationWizard(settings, () => policyRevision);

	let selectedProfileId = $state<string | null>(null);
	let shareTarget = $state<LibraryManagementProfile | null>(null);
	let importProfileOpen = $state(false);
	let managementHeading = $state<HTMLHeadingElement>();
	let activationDialog = $state<ReturnType<typeof LibraryManagementActivationDialog>>();

	// Resumes the signed-in admin's wizard once their settings have loaded; a
	// different admin starts clean.
	$effect(() => {
		const response = settings.query.data;
		const loaded = settings.draft !== undefined;
		const userId = authStore.user?.id ?? null;
		if (userId === untrack(() => wizard.sessionUser)) return;
		wizard.clear();
		wizard.sessionUser = null;
		if (!userId || !response || !loaded) return;
		wizard.restore(userId, response.settings_revision);
	});

	$effect(() => {
		wizard.persist(authStore.user?.id);
	});

	const saving = $derived(section.saving || validateSettings.isPending || impactSettings.isPending);
	const saveStatus = $derived(
		settings.error ? { tone: 'error' as const, message: settings.error } : section.status
	);
	const operationHistory = $derived(
		operationsQuery.data?.pages.flatMap((page) => page.items) ?? []
	);
	const remoteActivation = $derived(
		operationHistory.find(
			(item) =>
				item.activation_preview &&
				['queued', 'running', 'paused', 'ready'].includes(item.operation.state)
		) ?? null
	);
	const selectedProfile = $derived(
		settings.profiles.find((profile) => profile.id === selectedProfileId) ?? null
	);
	const staleActivationRootIds = $derived(
		activationHealthQuery.isLoading ? [] : (activationHealthQuery.data?.stale_root_ids ?? [])
	);
	const staleActivationRootLabels = $derived(staleActivationRootIds.map(rootLabel));
	const blockedActivationRootIds = $derived(
		activationHealthQuery.isLoading ? [] : (activationHealthQuery.data?.blocked_root_ids ?? [])
	);
	const blockedActivationRootLabels = $derived(blockedActivationRootIds.map(rootLabel));
	const blockedActivationReason = $derived(
		activationHealthQuery.isLoading ? null : (activationHealthQuery.data?.blocked_reason ?? null)
	);
	const currentActivationRoot = $derived(
		roots.find((root) => root.id === wizard.currentRootId) ?? null
	);
	const activationPreview = $derived(wizard.preview.data);

	function rootLabel(rootId: string): string {
		return roots.find((root) => root.id === rootId)?.label ?? 'Unknown root';
	}

	function rootManagementStatus(rootId: string): string {
		const saved = settings.persistedAssignmentFor(rootId);
		const current = settings.assignmentFor(rootId);
		if (JSON.stringify($state.snapshot(current)) !== JSON.stringify($state.snapshot(saved))) {
			return 'Pending unsaved changes';
		}
		if (writesAutomatically(saved)) {
			if (activationHealthQuery.isLoading) return 'Checking activation…';
			if (blockedActivationRootIds.includes(rootId)) return 'Paused: needs attention';
			return staleActivationRootIds.includes(rootId) ? 'Activation stale: run a dry run' : 'Active';
		}
		return saved.enabled ? 'Configured; automatic triggers off' : 'Off';
	}

	function checkFailure(error: unknown): string {
		if (error instanceof ApiError && error.status >= 400 && error.status < 500) {
			return error.message;
		}
		return 'Could not check the change. Try again.';
	}

	/**
	 * Checks the draft and saves it, or, when the change widens what automatic
	 * work writes, starts the activation wizard's dry runs instead. True when
	 * it saved or handed over to the wizard.
	 */
	async function reviewAndSave(opener: HTMLElement | null): Promise<boolean> {
		const proposed = settings.snapshot();
		if (!proposed) return false;
		settings.error = '';
		try {
			await validateSettings.mutateAsync({
				settings: proposed,
				expected_settings_revision: settings.revision
			});
			const impact = await impactSettings.mutateAsync({
				settings: proposed,
				expected_settings_revision: settings.revision
			});
			if (impact.stale) {
				settings.error = 'These settings changed elsewhere. Discard to load them.';
				return false;
			}
			const activeAffected = impact.affected_root_ids.filter((rootId) => {
				const assignment = proposed.root_assignments.find((value) => value.root_id === rootId);
				return Boolean(assignment && writesAutomatically(assignment));
			});
			if (impact.preview_required && activeAffected.length > 0) {
				wizard.begin(proposed, activeAffected);
				activationDialog?.show(opener);
				return true;
			}
		} catch (error) {
			settings.error = checkFailure(error);
			return false;
		}
		await section.save();
		if (section.status?.tone === 'error') return false;
		wizard.reset();
		return true;
	}

	/** Saves the draft, first stopping a dry run that no longer covers it. */
	async function saveChanges(opener: HTMLElement | null): Promise<void> {
		if (wizard.active && !wizard.coversDraft) {
			if (wizard.pending) return;
			if (wizard.canStop) {
				await wizard.stopRun();
				if (wizard.error) return;
			}
			wizard.reset();
		}
		await reviewAndSave(opener);
	}

	function focusedElement(): HTMLElement | null {
		return document.activeElement instanceof HTMLElement ? document.activeElement : null;
	}

	async function saveProfile(
		profile: LibraryManagementProfile,
		namingScripts: ManagementScriptSettings[],
		taggingScripts: ManagementScriptSettings[]
	): Promise<void> {
		const draft = settings.draft;
		const before = settings.snapshot();
		if (!draft || !before) return;
		draft.profiles = draft.profiles.map((value) => (value.id === profile.id ? profile : value));
		draft.naming_scripts = namingScripts;
		draft.tagging_scripts = taggingScripts;
		if (await reviewAndSave(null)) {
			selectedProfileId = null;
			return;
		}
		// A refused save leaves the change in the editor only, so closing it drops it.
		draft.profiles = before.profiles;
		draft.naming_scripts = before.naming_scripts;
		draft.tagging_scripts = before.tagging_scripts;
		throw new Error(saveStatus?.message ?? 'Could not save this profile.');
	}

	function acceptImportedProfile(result: LibraryManagementProfileImportResponse): void {
		settings.acceptImport(result);
		selectedProfileId = result.profile.id;
	}

	function startFreshActivation(opener: HTMLElement): void {
		if (
			!settings.persisted ||
			settings.dirty ||
			wizard.pending ||
			staleActivationRootIds.length === 0
		)
			return;
		wizard.beginFresh($state.snapshot(settings.persisted), [...staleActivationRootIds]);
		activationDialog?.show(opener);
	}

	function focusHeading(): void {
		managementHeading?.focus();
	}

	onMount(() => {
		if (!authStore.isAdmin) selectedProfileId = null;
	});
</script>

<UnsavedChangesGuard />

<section class="management-settings-shell" aria-labelledby="library-management-title">
	<header class="management-settings-header">
		<div class="management-write-mark"><FolderCog class="h-6 w-6" /></div>
		<div class="min-w-0 flex-1">
			<p class="management-kicker"><ShieldAlert class="h-3.5 w-3.5" /> Optional write access</p>
			<h2
				bind:this={managementHeading}
				id="library-management-title"
				tabindex="-1"
				class="font-display text-xl font-semibold"
			>
				Automation
			</h2>
			<p class="mt-1 text-sm text-base-content/65">
				Writes tags and moves files. It is separate from scanning and remains off until an
				administrator confirms a dry run.
			</p>
		</div>
		<span class="management-status-badge" data-active={settings.activeAssignments.length > 0}>
			{#if settings.activeAssignments.length > 0}<Check class="h-3.5 w-3.5" /> Active on {settings
					.activeAssignments.length}
				root{settings.activeAssignments.length === 1 ? '' : 's'}{:else}<CircleOff
					class="h-3.5 w-3.5"
				/> Off everywhere{/if}
		</span>
	</header>

	<QueryBoundary query={section} what="the automation settings">
		{#snippet loading()}
			<div class="space-y-3 p-5">
				<div class="skeleton h-28 rounded-xl"></div>
				<div class="skeleton h-48 rounded-xl"></div>
			</div>
		{/snippet}
		{#snippet children(draft)}
			<div class="space-y-6 p-5 sm:p-6">
				<LibraryManagementProfileList
					{settings}
					{roots}
					onedit={(profileId) => (selectedProfileId = profileId)}
					onshare={(profile) => (shareTarget = profile)}
					onimport={() => (importProfileOpen = true)}
					focusFallback={focusHeading}
				/>

				<section class="space-y-3" aria-labelledby="management-roots-title">
					<div>
						<p class="management-step">02 · Root automation</p>
						<h3 id="management-roots-title" class="font-display text-lg font-semibold">
							Choose where writes are allowed
						</h3>
						<p class="text-xs text-base-content/55">
							Scanning reads and identifies. These controls separately allow future writes in each
							root.
						</p>
					</div>
					<div class="space-y-3">
						{#each roots as root (root.id)}
							<LibraryManagementRootAssignment
								{root}
								{settings}
								status={rootManagementStatus(root.id)}
							/>
						{/each}
					</div>
				</section>

				<details class="management-retention-panel">
					<summary
						><span class="management-editor-icon"><History class="h-4 w-4" /></span><span
							class="flex flex-col"
							><strong>Retention and refresh</strong><small class="text-xs text-base-content/55"
								>Advanced recovery settings</small
							></span
						><ChevronRight class="ml-auto h-4 w-4 management-editor-chevron" /></summary
					>
					<div class="mt-4 grid gap-x-4 gap-y-2 sm:grid-cols-2">
						<NumberField
							label="Undo snapshots (days)"
							min={1}
							max={3650}
							bind:value={draft.undo_retention_days}
						/>
						<NumberField
							label="Preview lifetime (hours)"
							min={1}
							max={168}
							bind:value={draft.preview_retention_hours}
						/>
						<div class="py-2 sm:col-span-2">
							<ToggleRow
								label="Post-commit refresh"
								description="Refresh DroppedNeedle and selected media servers."
								bind:checked={draft.external_refresh.enabled}
							/>
						</div>
						{#if draft.external_refresh.enabled}
							<div
								class="management-choice-grid sm:col-span-2"
								aria-label="External refresh targets"
							>
								<div class="management-trigger">
									<ToggleRow label="Plex" bind:checked={draft.external_refresh.plex_enabled} />
								</div>
								<div class="management-trigger">
									<ToggleRow
										label="Jellyfin"
										bind:checked={draft.external_refresh.jellyfin_enabled}
									/>
								</div>
								<div class="management-trigger">
									<ToggleRow
										label="Navidrome"
										bind:checked={draft.external_refresh.navidrome_enabled}
									/>
								</div>
							</div>
						{/if}
						<LibraryManagementPurgeDialog onerror={(message) => (settings.error = message)} />
					</div>
				</details>

				<div class="management-save-bar" aria-live="polite">
					{#if wizard.active}
						<div class="management-save-bar__notice">
							<div>
								<strong class="text-sm">
									{#if !wizard.coversDraft}Configuration changed after this dry run started{:else if !currentActivationRoot}Activation
										awaits final confirmation{:else if wizard.ready}Dry run ready for review{:else if activationPreview?.control_request === 'stop'}Dry
										run stopping{:else if wizard.stalled}Dry run interrupted{:else if activationPreview?.state === 'queued'}Dry
										run queued{:else if activationPreview?.state === 'running'}Dry run planning{:else if ['failed', 'stopped', 'cancelled'].includes(activationPreview?.state ?? '')}Dry
										run needs attention{:else}Activation dry run continues{/if}
								</strong>
								<p class="text-xs text-base-content/50">
									{#if !wizard.coversDraft}The existing result cannot authorize the settings now
										shown. Restart the review to create one current dry run.{:else if currentActivationRoot}{currentActivationRoot.label}
										{#if (activationPreview?.summary.item_count ?? 0) > 0}
											· {(activationPreview?.summary.item_count ?? 0).toLocaleString()}
											files found · {(
												activationPreview?.summary.bundle_count ?? 0
											).toLocaleString()}
											release bundles{:else}
											· Discovering files and release bundles{/if}{:else}Every required dry run is
										accepted. Automatic writes are still off.{/if}
								</p>
							</div>
							{#if !wizard.coversDraft}<Button
									variant="warning"
									size="sm"
									icon={RefreshCw}
									disabled={wizard.pending}
									onclick={(event: MouseEvent & { currentTarget: HTMLElement }) =>
										void saveChanges(event.currentTarget)}>Restart review</Button
								>
							{:else}<Button
									variant="warning"
									size="sm"
									onclick={(event: MouseEvent & { currentTarget: HTMLElement }) =>
										activationDialog?.show(event.currentTarget)}
									>{#if currentActivationRoot && activationPreview?.state === 'running' && !wizard.stalled}<span
											class="loading loading-spinner loading-sm"
											aria-hidden="true"
										></span>{/if}<ShieldAlert class="h-4 w-4" /> View dry run</Button
								>
							{/if}
						</div>
					{:else if remoteActivation}
						<div class="management-save-bar__notice">
							<div>
								<strong class="text-sm">
									{remoteActivation.operation.state === 'ready'
										? 'Write-access dry run ready'
										: 'Write-access dry run in progress'}
								</strong>
								<p class="text-xs text-base-content/50">
									{roots.find((root) => root.id === remoteActivation.target_root_id)?.label ??
										'This root'} · {remoteActivation.profile_name} · {remoteActivation.operation
										.state === 'ready'
										? 'Ready to review'
										: 'Discovering files and release bundles'}
								</p>
							</div>
							<Button
								variant="warning"
								size="sm"
								icon={ShieldAlert}
								href={`/library/management/previews/${encodeURIComponent(remoteActivation.operation.id)}`}
								>Review dry run</Button
							>
						</div>
					{:else if activationHealthQuery.isError}
						<div class="management-save-bar__notice">
							<div>
								<strong class="text-sm">Could not check activation status</strong>
								<p class="text-xs text-base-content/50">
									Automatic writes may be paused. Retry the status check.
								</p>
							</div>
							<Button
								variant="warning"
								size="sm"
								icon={RefreshCw}
								disabled={activationHealthQuery.isFetching}
								onclick={() => void activationHealthQuery.refetch()}>Retry status check</Button
							>
						</div>
					{:else if activationHealthQuery.isLoading}
						<div class="management-save-bar__notice">
							<div>
								<strong class="text-sm">Checking activation status…</strong>
								<p class="text-xs text-base-content/50">
									Confirming every automatic root still matches its dry run.
								</p>
							</div>
						</div>
					{:else if blockedActivationRootIds.length > 0}
						<div class="management-save-bar__notice">
							<div>
								<strong class="text-sm">Automatic writes paused: needs attention</strong>
								<p class="text-xs text-base-content/50">
									{#if blockedActivationRootLabels.length === 1}
										{blockedActivationRootLabels[0]} needs attention before automatic writes can resume.
										Check the library roots and the assignment below, then recheck.
									{:else}
										{blockedActivationRootLabels.length} roots need attention before automatic writes
										can resume. Check the library roots and assignments below, then recheck.
									{/if}
								</p>
								{#if blockedActivationReason}
									<p class="mt-1 text-xs text-base-content/50">{blockedActivationReason}</p>
								{/if}
							</div>
							<Button
								variant="warning"
								size="sm"
								icon={RefreshCw}
								disabled={activationHealthQuery.isFetching}
								onclick={() => void activationHealthQuery.refetch()}>Recheck</Button
							>
						</div>
					{:else if staleActivationRootIds.length > 0}
						<div class="management-save-bar__notice">
							<div>
								<strong class="text-sm">Automatic writes paused: dry run needed</strong>
								<p class="text-xs text-base-content/50">
									{#if staleActivationRootLabels.length === 1}
										{staleActivationRootLabels[0]} no longer matches its confirmed dry run. Run a fresh
										dry run to resume automatic writes.
									{:else}
										{staleActivationRootLabels.length} roots no longer match their confirmed dry run.
										Run a fresh dry run to resume automatic writes.
									{/if}
								</p>
							</div>
							<Button
								variant="warning"
								size="sm"
								icon={ShieldAlert}
								disabled={wizard.pending || activationHealthQuery.isFetching || settings.dirty}
								onclick={(event: MouseEvent & { currentTarget: HTMLElement }) =>
									startFreshActivation(event.currentTarget)}>Run a fresh dry run</Button
							>
						</div>
					{/if}
					<SaveBar
						{...section.bar}
						{saving}
						status={saveStatus}
						saveLabel="Validate and save"
						onsave={() => saveChanges(focusedElement())}
						ondiscard={() => {
							settings.error = '';
							section.discard();
						}}
					/>
				</div>
			</div>
		{/snippet}
	</QueryBoundary>
</section>

{#if selectedProfile}
	<LibraryManagementProfileEditor
		profile={selectedProfile}
		managedFields={settings.query.data?.managed_fields ?? []}
		namingScripts={settings.draft?.naming_scripts ?? []}
		taggingScripts={settings.draft?.tagging_scripts ?? []}
		{saving}
		onclose={() => (selectedProfileId = null)}
		onsave={saveProfile}
	/>
{/if}

<LibraryManagementProfileSharing
	shareProfile={shareTarget}
	importOpen={importProfileOpen}
	settingsRevision={settings.revision}
	onshareclose={() => (shareTarget = null)}
	onimportclose={() => (importProfileOpen = false)}
	onimported={acceptImportedProfile}
/>

<LibraryManagementActivationDialog
	bind:this={activationDialog}
	{wizard}
	{roots}
	focusFallback={focusHeading}
/>

<style>
	.management-settings-shell {
		overflow: hidden;
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 28%, transparent);
		border-radius: var(--radius-box);
		background:
			linear-gradient(
				160deg,
				color-mix(in oklab, var(--color-library-manage) 5%, var(--color-base-100)),
				var(--color-base-100) 18rem
			),
			var(--color-base-100);
		box-shadow: 0 1rem 3rem color-mix(in oklab, var(--color-base-content) 5%, transparent);
	}

	.management-settings-header {
		display: flex;
		align-items: flex-start;
		gap: 1rem;
		padding: 1.25rem;
		border-bottom: 1px solid color-mix(in oklab, var(--color-library-manage) 18%, transparent);
		background: color-mix(in oklab, var(--color-library-manage) 7%, transparent);
	}

	.management-retention-panel {
		padding: 1rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 11%, transparent);
		border-radius: 0.9rem;
		background: color-mix(in oklab, var(--color-base-100) 88%, var(--color-base-200));
	}

	.management-retention-panel > summary {
		display: flex;
		align-items: center;
		gap: 0.7rem;
		list-style: none;
		cursor: pointer;
	}

	.management-retention-panel > summary::-webkit-details-marker {
		display: none;
	}

	.management-save-bar {
		position: sticky;
		bottom: 0.75rem;
		z-index: 2;
		display: grid;
		gap: 0.75rem;
		padding: 0.85rem 1rem;
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 22%, transparent);
		border-radius: 0.9rem;
		background: color-mix(in oklab, var(--color-base-100) 92%, transparent);
		box-shadow: 0 0.75rem 2rem color-mix(in oklab, var(--color-base-content) 8%, transparent);
		backdrop-filter: blur(0.75rem);
	}

	.management-save-bar__notice {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding-bottom: 0.75rem;
		border-bottom: 1px solid color-mix(in oklab, var(--color-base-content) 10%, transparent);
	}

	@media (max-width: 639px) {
		.management-settings-header {
			align-items: stretch;
			flex-direction: column;
		}

		.management-save-bar__notice {
			align-items: stretch;
			flex-direction: column;
		}
	}
</style>
