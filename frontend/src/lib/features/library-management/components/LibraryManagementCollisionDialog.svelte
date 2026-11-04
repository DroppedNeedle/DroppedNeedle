<!--
	Chooses how to resolve one destination collision. Nothing is preselected,
	and the choice never writes: it creates a fresh resolution preview that
	rechecks both files, and the page moves to it.
-->
<script lang="ts">
	import '$lib/features/library-management/components/library-management.css';
	import { goto } from '$app/navigation';
	import { HardDrive } from 'lucide-svelte';

	import { createLibraryManagementDuplicateResolutionMutation } from '$lib/features/library-management/queries/LibraryManagementMutations.svelte';
	import { rememberLibraryManagementPreviewToken } from '$lib/features/library-management/queries/LibraryManagementPreviewTokens';
	import type {
		DuplicateResolutionAction,
		LibraryManagementPlanItem,
		LibraryManagementPreviewDetailResponse
	} from '$lib/features/library-management/queries/types';
	import Button from '$lib/ui/Button.svelte';
	import Input from '$lib/ui/Input.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { createUuid } from '$lib/utils/uuid';
	import {
		managementDisplayPath,
		titleManagementValue,
		type ManagementCollision
	} from './LibraryManagementDisplay';

	interface Props {
		jobId: string;
		preview: LibraryManagementPreviewDetailResponse;
		roots: ReadonlyArray<{ id: string; label: string }>;
		settingsRevision: string | null;
		policyRevision: string | null;
	}

	let { jobId, preview, roots, settingsRevision, policyRevision }: Props = $props();

	interface Selection {
		item: LibraryManagementPlanItem;
		collision: ManagementCollision;
	}

	const resolutions: Array<{
		value: DuplicateResolutionAction;
		title: string;
		detail: string;
		recycles: boolean;
	}> = [
		{
			value: 'keep_existing',
			title: 'Keep existing; leave incoming in place',
			detail: 'Creates a no-write resolution preview for the incoming destination.',
			recycles: false
		},
		{
			value: 'keep_incoming_alternate',
			title: 'Keep incoming at an alternate relative path',
			detail: 'Both files remain.',
			recycles: false
		},
		{
			value: 'recycle_existing_keep_incoming',
			title: 'Recycle existing; keep incoming',
			detail: 'Moves the existing file to the configured recycle area.',
			recycles: true
		},
		{
			value: 'recycle_incoming_keep_existing',
			title: 'Recycle incoming; keep existing',
			detail: 'Moves the incoming file to the configured recycle area.',
			recycles: true
		}
	];

	let shown = $state(false);
	let selection = $state<Selection | null>(null);
	let action = $state<DuplicateResolutionAction | ''>('');
	let alternateRelativePath = $state('');
	let error = $state('');

	const createResolution = createLibraryManagementDuplicateResolutionMutation();
	const requestReady = $derived(
		Boolean(
			selection?.collision.requestKind &&
			selection.collision.existingRootId &&
			selection.collision.existingRelativePath &&
			action &&
			(action !== 'keep_incoming_alternate' || alternateRelativePath.trim())
		)
	);

	export function open(item: LibraryManagementPlanItem, collision: ManagementCollision): void {
		selection = { item, collision };
		action = '';
		alternateRelativePath = '';
		error = '';
		shown = true;
	}

	async function resolve(): Promise<void> {
		const current = selection;
		if (
			!current?.collision.requestKind ||
			!current.collision.existingRootId ||
			!current.collision.existingRelativePath ||
			!action ||
			!settingsRevision ||
			!policyRevision ||
			!requestReady
		) {
			return;
		}
		error = '';
		try {
			const handle = await createResolution.mutateAsync({
				source_job_id: jobId,
				source_plan_item_ordinal: current.item.ordinal,
				expected_source_operation_row_revision: preview.operation_row_revision,
				collision_kind: current.collision.requestKind,
				existing_root_id: current.collision.existingRootId,
				existing_relative_path: current.collision.existingRelativePath,
				action,
				expected_settings_revision: settingsRevision,
				expected_policy_revision: policyRevision,
				idempotency_key: createUuid(),
				existing_local_track_id: current.collision.existingLocalTrackId,
				alternate_relative_path:
					action === 'keep_incoming_alternate' ? alternateRelativePath.trim() : null
			});
			rememberLibraryManagementPreviewToken(handle.job_id, handle.preview_token);
			shown = false;
			await goto(withBasePath(`/library/management/previews/${encodeURIComponent(handle.job_id)}`));
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not create a resolution preview.';
		}
	}
</script>

<Modal
	bind:open={shown}
	title="Choose a collision resolution"
	description="No option is preselected. DroppedNeedle rechecks both files before it offers another Apply action."
	size="lg"
	focusTitle
	closeButton
	dismissible={!createResolution.isPending}
>
	{#if selection}<div class="rounded-xl border border-base-content/10 p-3 text-sm">
			<p>
				<strong>Incoming:</strong>
				{managementDisplayPath(
					roots,
					selection.item.source_root_id ?? null,
					selection.item.source_relative_path ?? null
				)}
			</p>
			<p class="mt-1">
				<strong>Existing:</strong>
				{managementDisplayPath(
					roots,
					selection.collision.existingRootId,
					selection.collision.existingRelativePath
				)}
			</p>
			<p class="mt-1">
				<strong>Evidence:</strong>
				{titleManagementValue(selection.collision.classification)}
			</p>
		</div>{/if}
	<fieldset class="mt-4 grid gap-2">
		<legend class="mb-2 text-sm font-semibold">Resolution</legend>
		{#each resolutions as option (option.value)}
			<label class="management-selection-card"
				><input
					type="radio"
					class="radio radio-sm"
					name="collision-action"
					value={option.value}
					bind:group={action}
				/><span><strong>{option.title}</strong><small>{option.detail}</small></span></label
			>
		{/each}
	</fieldset>
	{#if action === 'keep_incoming_alternate'}<label class="mt-3 grid gap-1 text-sm"
			><span>Alternate relative path</span><Input
				label="Alternate relative path"
				mono
				bind:value={alternateRelativePath}
				placeholder="Artist/Album/02 Track (alternate).flac"
			/><small class="text-muted"
				>Relative to the planned destination root; absolute paths are rejected.</small
			></label
		>{/if}
	{#if error}<div class="alert alert-error mt-3 text-sm" role="alert">
			{error}
		</div>{/if}
	{#snippet actions()}
		<Button variant="ghost" disabled={createResolution.isPending} onclick={() => (shown = false)}
			>Cancel</Button
		><Button
			variant="warning"
			icon={HardDrive}
			loading={createResolution.isPending}
			disabled={!requestReady}
			onclick={() => void resolve()}>Generate resolution preview</Button
		>
	{/snippet}
</Modal>
