<!--
	Chooses how to resolve one destination collision. Nothing is preselected,
	and the choice never writes: it creates a fresh resolution preview that
	rechecks both files, and the page moves to it.
-->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { goto } from '$app/navigation';
	import { HardDrive, X } from 'lucide-svelte';

	import { createLibraryManagementDuplicateResolutionMutation } from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import { rememberLibraryManagementPreviewToken } from '$lib/queries/library-management/LibraryManagementPreviewTokens';
	import type {
		DuplicateResolutionAction,
		LibraryManagementPlanItem,
		LibraryManagementPreviewDetailResponse
	} from '$lib/queries/library-management/types';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
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
		recycleAvailable: boolean;
	}

	let { jobId, preview, roots, settingsRevision, policyRevision, recycleAvailable }: Props =
		$props();

	interface Selection {
		item: LibraryManagementPlanItem;
		collision: ManagementCollision;
	}

	const actions: Array<{
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

	let dialog: HTMLDialogElement;
	let heading: HTMLHeadingElement;
	let opener: HTMLElement | null = null;
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
			(!action.startsWith('recycle_') || recycleAvailable) &&
			(action !== 'keep_incoming_alternate' || alternateRelativePath.trim())
		)
	);

	export function open(
		item: LibraryManagementPlanItem,
		collision: ManagementCollision,
		button: HTMLElement
	): void {
		opener = button;
		selection = { item, collision };
		action = '';
		alternateRelativePath = '';
		error = '';
		dialog.showModal();
		heading.focus();
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
			dialog.close();
			await goto(withBasePath(`/library/management/previews/${encodeURIComponent(handle.job_id)}`));
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not create a resolution preview.';
		}
	}
</script>

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby="resolve-management-collision"
	onclose={() => opener?.focus()}
	oncancel={(event) => {
		if (createResolution.isPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-2xl border border-error/25">
		<div class="flex items-start justify-between gap-3">
			<div>
				<p class="management-kicker">Fresh preview required</p>
				<h2
					bind:this={heading}
					id="resolve-management-collision"
					tabindex="-1"
					class="font-display text-xl font-semibold"
				>
					Choose a collision resolution
				</h2>
				<p class="mt-1 text-sm text-base-content/55">
					No option is preselected. DroppedNeedle rechecks both files before it offers another Apply
					action.
				</p>
			</div>
			<IconButton
				size="sm"
				shape="square"
				icon={X}
				label="Close collision resolution"
				disabled={createResolution.isPending}
				onclick={() => dialog.close()}
			/>
		</div>
		{#if selection}<div class="mt-4 rounded-xl border border-base-content/10 p-3 text-sm">
				<p>
					<strong>Incoming:</strong>
					{managementDisplayPath(
						roots,
						selection.item.source_root_id,
						selection.item.source_relative_path
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
			{#each actions as option (option.value)}
				<label class="management-selection-card"
					><input
						type="radio"
						class="radio radio-sm"
						name="collision-action"
						value={option.value}
						bind:group={action}
						disabled={option.recycles && !recycleAvailable}
					/><span
						><strong>{option.title}</strong><small
							>{option.recycles && !recycleAvailable
								? 'Unavailable until a recycle directory is configured.'
								: option.detail}</small
						></span
					></label
				>
			{/each}
		</fieldset>
		{#if action === 'keep_incoming_alternate'}<label class="mt-3 grid gap-1 text-sm"
				><span>Alternate relative path</span><Input
					label="Alternate relative path"
					mono
					bind:value={alternateRelativePath}
					placeholder="Artist/Album/02 Track (alternate).flac"
				/><small class="text-base-content/45"
					>Relative to the planned destination root; absolute paths are rejected.</small
				></label
			>{/if}
		{#if error}<div class="alert alert-error mt-3 text-sm" role="alert">
				{error}
			</div>{/if}
		<div class="modal-action">
			<Button variant="ghost" disabled={createResolution.isPending} onclick={() => dialog.close()}
				>Cancel</Button
			><Button
				variant="warning"
				icon={HardDrive}
				loading={createResolution.isPending}
				disabled={!requestReady}
				onclick={() => void resolve()}>Generate resolution preview</Button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Cancel collision resolution" disabled={createResolution.isPending}
			>close</button
		>
	</form>
</dialog>
