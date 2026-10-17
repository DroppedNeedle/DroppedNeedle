<!--
	The Automation tab of Library Management: profiles, where writes are
	allowed, retention, and the save bar. Saving checks the change first; a
	change that widens what automatic work writes goes through the activation
	wizard's dry runs instead of saving straight away.
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
		ShieldAlert,
		Sparkles
	} from 'lucide-svelte';

	import LibraryManagementActivationDialog from './LibraryManagementActivationDialog.svelte';
	import LibraryManagementProfileEditor from './LibraryManagementProfileEditor.svelte';
	import LibraryManagementProfileList from './LibraryManagementProfileList.svelte';
	import LibraryManagementProfileSharing from './LibraryManagementProfileSharing.svelte';
	import LibraryManagementPurgeDialog from './LibraryManagementPurgeDialog.svelte';
	import LibraryManagementRootAssignment from './LibraryManagementRootAssignment.svelte';
	import { ActivationWizard } from './activationWizard.svelte';
	import { ManagementSettingsDraft, writesAutomatically } from './managementSettingsDraft.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { LibraryRootSettings } from '$lib/queries/library/LibraryOperationsTypes';
	import {
		getLibraryManagementActivationHealthQuery,
		getLibraryManagementOperationsQuery,
		getLibraryManagementSettingsQuery
	} from '$lib/queries/library-management/LibraryManagementQueries.svelte';
	import {
		previewLibraryManagementSettingsImpactMutation,
		updateLibraryManagementSettingsMutation,
		validateLibraryManagementSettingsMutation
	} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import type {
		LibraryManagementProfile,
		LibraryManagementProfileImportResponse,
		LibraryManagementSettings,
		ManagementScriptSettings
	} from '$lib/queries/library-management/types';
	import Button from '$lib/ui/Button.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		roots: LibraryRootSettings[];
		policyRevision: string;
	}

	let { roots, policyRevision }: Props = $props();
	const settingsQuery = getLibraryManagementSettingsQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const operationsQuery = getLibraryManagementOperationsQuery(
		() => authStore.user?.id,
		() => ({ limit: 20 })
	);
	const activationHealthQuery = getLibraryManagementActivationHealthQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const updateSettings = updateLibraryManagementSettingsMutation();
	const validateSettings = validateLibraryManagementSettingsMutation();
	const impactSettings = previewLibraryManagementSettingsImpactMutation();

	const settings = new ManagementSettingsDraft();
	const wizard = new ActivationWizard(settings, () => policyRevision);

	let selectedProfileId = $state<string | null>(null);
	let shareTarget = $state<LibraryManagementProfile | null>(null);
	let importProfileOpen = $state(false);
	let managementHeading = $state<HTMLHeadingElement>();
	let activationDialog = $state<ReturnType<typeof LibraryManagementActivationDialog>>();

	// Loads the settings for the signed-in admin and resumes their wizard; a
	// different admin starts clean. After that, a newer revision replaces the draft.
	$effect(() => {
		const response = settingsQuery.data;
		const userId = authStore.user?.id ?? null;
		if (userId !== untrack(() => wizard.sessionUser)) {
			wizard.clear();
			wizard.sessionUser = null;
			if (!userId || !response) return;
			settings.load(response);
			wizard.restore(userId, response.settings_revision);
			return;
		}
		if (response) settings.sync(response);
	});

	$effect(() => {
		wizard.persist(authStore.user?.id);
	});

	const saving = $derived(
		updateSettings.isPending || validateSettings.isPending || impactSettings.isPending
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

	async function reviewAndSave(
		proposed: LibraryManagementSettings,
		opener: HTMLElement | null = null
	): Promise<void> {
		settings.saveError = '';
		try {
			await validateSettings.mutateAsync({
				settings: proposed,
				expected_settings_revision: settings.sourceRevision
			});
			const impact = await impactSettings.mutateAsync({
				settings: proposed,
				expected_settings_revision: settings.sourceRevision
			});
			if (impact.stale) throw new Error('Organization settings changed. Reload and try again.');
			const activeAffected = impact.affected_root_ids.filter((rootId) => {
				const assignment = proposed.root_assignments.find((value) => value.root_id === rootId);
				return Boolean(assignment && writesAutomatically(assignment));
			});
			if (impact.preview_required && activeAffected.length > 0) {
				wizard.begin(proposed, activeAffected);
				activationDialog?.show(opener);
				return;
			}
			const saved = await updateSettings.mutateAsync({
				settings: proposed,
				expected_settings_revision: settings.sourceRevision
			});
			settings.accept(saved);
			wizard.reset();
		} catch (error) {
			settings.saveError =
				error instanceof Error ? error.message : 'Could not save organization settings.';
			throw error;
		}
	}

	async function saveProfile(
		profile: LibraryManagementProfile,
		namingScripts: ManagementScriptSettings[],
		taggingScripts: ManagementScriptSettings[]
	): Promise<void> {
		if (!settings.draft) return;
		const proposed = {
			...$state.snapshot(settings.draft),
			profiles: settings.draft.profiles.map((value) => (value.id === profile.id ? profile : value)),
			naming_scripts: namingScripts,
			tagging_scripts: taggingScripts
		};
		await reviewAndSave(proposed);
		selectedProfileId = null;
	}

	function acceptImportedProfile(result: LibraryManagementProfileImportResponse): void {
		if (!settings.draft) return;
		settings.acceptImport(result);
		selectedProfileId = result.profile.id;
	}

	async function restartActivationReview(opener: HTMLElement): Promise<void> {
		if (!settings.draft || wizard.pending) return;
		if (wizard.canStop) {
			await wizard.stopRun();
			if (wizard.error) return;
		}
		const proposed = $state.snapshot(settings.draft);
		wizard.reset();
		await reviewAndSave(proposed, opener).catch(() => undefined);
	}

	function startFreshActivation(opener: HTMLElement): void {
		if (
			!settings.persisted ||
			settings.hasUnsaved ||
			wizard.pending ||
			staleActivationRootIds.length === 0
		)
			return;
		wizard.beginFresh($state.snapshot(settings.persisted), [...staleActivationRootIds]);
		activationDialog?.show(opener);
	}

	function saveDraft(event: MouseEvent & { currentTarget: HTMLElement }): void {
		if (!settings.draft) return;
		void reviewAndSave($state.snapshot(settings.draft), event.currentTarget).catch(() => undefined);
	}

	function focusHeading(): void {
		managementHeading?.focus();
	}

	onMount(() => {
		if (!authStore.isAdmin) selectedProfileId = null;
	});
</script>

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

	{#if settingsQuery.isLoading}
		<div class="space-y-3 p-5">
			<div class="skeleton h-28 rounded-xl"></div>
			<div class="skeleton h-48 rounded-xl"></div>
		</div>
	{:else if settingsQuery.isError || !settings.draft}
		<div class="m-5 alert alert-error">Could not load organization settings.</div>
	{:else}
		{@const draft = settings.draft}
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
						><strong>Retention, recycle, and refresh</strong><small
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
					<div class="sm:col-span-2">
						<TextField
							label="Recycle directory"
							mono
							placeholder="/srv/music-recycle"
							help="A recovery destination, not a scanned library root. Recycle actions stay unavailable while empty or unsafe."
							bind:value={draft.recycle_bin_path}
						/>
					</div>
					<div class="py-2 sm:col-span-2">
						<ToggleRow
							label="Post-commit refresh"
							description="Refresh DroppedNeedle and selected media servers."
							bind:checked={draft.external_refresh.enabled}
						/>
					</div>
					{#if draft.external_refresh.enabled}
						<div class="management-choice-grid sm:col-span-2" aria-label="External refresh targets">
							<label
								><input
									type="checkbox"
									class="checkbox checkbox-sm"
									bind:checked={draft.external_refresh.plex_enabled}
								/><span>Plex</span></label
							>
							<label
								><input
									type="checkbox"
									class="checkbox checkbox-sm"
									bind:checked={draft.external_refresh.jellyfin_enabled}
								/><span>Jellyfin</span></label
							>
							<label
								><input
									type="checkbox"
									class="checkbox checkbox-sm"
									bind:checked={draft.external_refresh.navidrome_enabled}
								/><span>Navidrome</span></label
							>
						</div>
						<NumberField
							label="Refresh retry attempts"
							min={0}
							max={20}
							bind:value={draft.external_refresh.retry_attempts}
						/>
						<NumberField
							label="Retry delay (seconds)"
							min={0}
							max={3600}
							bind:value={draft.external_refresh.retry_delay_seconds}
						/>
					{/if}
					<LibraryManagementPurgeDialog onerror={(message) => (settings.saveError = message)} />
				</div>
			</details>

			{#if settings.saveError}<div class="alert alert-error text-sm" role="alert">
					{settings.saveError}
				</div>{/if}
			<div class="management-save-bar" aria-live="polite">
				{#if wizard.active}
					<div>
						<strong class="text-sm">
							{#if !wizard.coversDraft}Configuration changed after this dry run started{:else if !currentActivationRoot}Activation
								awaits final confirmation{:else if wizard.ready}Dry run ready for review{:else if activationPreview?.control_request === 'stop'}Dry
								run stopping{:else if wizard.stalled}Dry run interrupted{:else if activationPreview?.state === 'queued'}Dry
								run queued{:else if activationPreview?.state === 'running'}Dry run planning{:else if ['failed', 'stopped', 'cancelled'].includes(activationPreview?.state ?? '')}Dry
								run needs attention{:else}Activation dry run continues{/if}
						</strong>
						<p class="text-xs text-base-content/50">
							{#if !wizard.coversDraft}The existing result cannot authorize the settings now shown.
								Restart the review to create one current dry run.{:else if currentActivationRoot}{currentActivationRoot.label}
								{#if (activationPreview?.summary.item_count ?? 0) > 0}
									· {(activationPreview?.summary.item_count ?? 0).toLocaleString()}
									files found · {(activationPreview?.summary.bundle_count ?? 0).toLocaleString()}
									release bundles{:else}
									· Discovering files and release bundles{/if}{:else}Every required dry run is
								accepted. Automatic writes are still off.{/if}
						</p>
					</div>
					{#if !wizard.coversDraft}<Button
							variant="warning"
							icon={RefreshCw}
							disabled={wizard.pending}
							onclick={(event: MouseEvent & { currentTarget: HTMLElement }) =>
								void restartActivationReview(event.currentTarget)}>Restart review</Button
						>
					{:else}<Button
							variant="warning"
							onclick={(event: MouseEvent & { currentTarget: HTMLElement }) =>
								activationDialog?.show(event.currentTarget)}
							>{#if currentActivationRoot && activationPreview?.state === 'running' && !wizard.stalled}<span
									class="loading loading-spinner loading-sm"
									aria-hidden="true"
								></span>{/if}<ShieldAlert class="h-4 w-4" /> View dry run</Button
						>
					{/if}
				{:else if settings.hasUnsaved}
					<div>
						<strong class="text-sm">Review configuration changes</strong>
						<p class="text-xs text-base-content/50">
							Trigger-only changes save immediately. Changes to file-writing scope require a current
							dry run.
						</p>
					</div>
					<Button variant="warning" icon={Sparkles} loading={saving} onclick={saveDraft}
						>Validate and save</Button
					>
				{:else if remoteActivation}
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
						icon={ShieldAlert}
						href={`/library/management/previews/${encodeURIComponent(remoteActivation.operation.id)}`}
						>Review dry run</Button
					>
				{:else if activationHealthQuery.isError}
					<div>
						<strong class="text-sm">Could not check activation status</strong>
						<p class="text-xs text-base-content/50">
							Automatic writes may be paused. Retry the status check.
						</p>
					</div>
					<Button
						variant="warning"
						icon={RefreshCw}
						disabled={activationHealthQuery.isFetching}
						onclick={() => void activationHealthQuery.refetch()}>Retry status check</Button
					>
				{:else if activationHealthQuery.isLoading}
					<div>
						<strong class="text-sm">Checking activation status…</strong>
						<p class="text-xs text-base-content/50">
							Confirming every automatic root still matches its dry run.
						</p>
					</div>
					<Button variant="ghost" loading>Checking</Button>
				{:else if blockedActivationRootIds.length > 0}
					<div>
						<strong class="text-sm">Automatic writes paused: needs attention</strong>
						<p class="text-xs text-base-content/50">
							{#if blockedActivationRootLabels.length === 1}
								{blockedActivationRootLabels[0]} needs attention before automatic writes can resume. Check
								the library roots and the assignment below, then recheck.
							{:else}
								{blockedActivationRootLabels.length} roots need attention before automatic writes can
								resume. Check the library roots and assignments below, then recheck.
							{/if}
						</p>
						{#if blockedActivationReason}
							<p class="mt-1 text-xs text-base-content/50">{blockedActivationReason}</p>
						{/if}
					</div>
					<Button
						variant="warning"
						icon={RefreshCw}
						disabled={activationHealthQuery.isFetching}
						onclick={() => void activationHealthQuery.refetch()}>Recheck</Button
					>
				{:else if staleActivationRootIds.length > 0}
					<div>
						<strong class="text-sm">Automatic writes paused: dry run needed</strong>
						<p class="text-xs text-base-content/50">
							{#if staleActivationRootLabels.length === 1}
								{staleActivationRootLabels[0]} no longer matches its confirmed dry run. Run a fresh dry
								run to resume automatic writes.
							{:else}
								{staleActivationRootLabels.length} roots no longer match their confirmed dry run. Run
								a fresh dry run to resume automatic writes.
							{/if}
						</p>
					</div>
					<Button
						variant="warning"
						icon={ShieldAlert}
						disabled={wizard.pending || activationHealthQuery.isFetching}
						onclick={(event: MouseEvent & { currentTarget: HTMLElement }) =>
							startFreshActivation(event.currentTarget)}>Run a fresh dry run</Button
					>
				{:else}
					<div>
						<strong class="text-sm">Configuration saved</strong>
						<p class="text-xs text-base-content/50">
							Change a profile, assignment, or automatic trigger to review an update.
						</p>
					</div>
					<Button variant="ghost" icon={Check} disabled>Saved</Button>
				{/if}
			</div>
		</div>
	{/if}
</section>

{#if selectedProfile}
	<LibraryManagementProfileEditor
		profile={selectedProfile}
		managedFields={settingsQuery.data?.managed_fields ?? []}
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
	settingsRevision={settings.sourceRevision}
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
		border: 1px solid color-mix(in oklab, var(--color-base-content) 11%, transparent);
		border-radius: 0.9rem;
		background: color-mix(in oklab, var(--color-base-100) 88%, var(--color-base-200));
	}

	.management-save-bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.85rem 1rem;
		border: 1px dashed color-mix(in oklab, var(--color-base-content) 16%, transparent);
		border-radius: 0.9rem;
		background: color-mix(in oklab, var(--color-base-200) 58%, transparent);
	}

	.management-retention-panel {
		padding: 1rem;
	}

	.management-retention-panel > summary {
		align-items: center;
		gap: 0.7rem;
		list-style: none;
		cursor: pointer;
	}

	.management-retention-panel > summary {
		display: flex;
	}

	.management-retention-panel > summary::-webkit-details-marker {
		display: none;
	}

	.management-save-bar {
		position: sticky;
		bottom: 0.75rem;
		z-index: 2;
		border-style: solid;
		border-color: color-mix(in oklab, var(--color-library-manage) 22%, transparent);
		background: color-mix(in oklab, var(--color-base-100) 92%, transparent);
		box-shadow: 0 0.75rem 2rem color-mix(in oklab, var(--color-base-content) 8%, transparent);
		backdrop-filter: blur(0.75rem);
	}

	@media (max-width: 639px) {
		.management-settings-header {
			align-items: stretch;
			flex-direction: column;
		}

		.management-save-bar {
			align-items: stretch;
			flex-direction: column;
		}
	}
</style>
