<!--
	The write-access gate: a dry run for each affected root in turn, then the
	typed phrase that turns file organization on. It renders the activation
	wizard; the tab opens it with show().
-->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { Check, ChevronRight, Clock3, RefreshCw, ShieldAlert, Square } from 'lucide-svelte';

	import { LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE } from '$lib/queries/library-management/LibraryManagementConfirmation';
	import type { LibraryRootSettings } from '$lib/queries/library/LibraryOperationsTypes';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';

	import type { ActivationWizard } from './activationWizard.svelte';
	import { profileAspects } from './managementSettingsDraft.svelte';

	interface Props {
		wizard: ActivationWizard;
		roots: LibraryRootSettings[];
		/** Where focus goes when the dialog closes and its opener is gone. */
		focusFallback: () => void;
	}

	let { wizard, roots, focusFallback }: Props = $props();

	let dialog: HTMLDialogElement;
	let heading: HTMLHeadingElement;
	let opener: HTMLElement | null = null;

	const currentRoot = $derived(roots.find((root) => root.id === wizard.currentRootId) ?? null);
	const preview = $derived(wizard.preview.data);

	export function show(from: HTMLElement | null): void {
		wizard.phrase = '';
		opener = from;
		dialog.showModal();
		heading.focus();
	}

	async function enable(): Promise<void> {
		if (await wizard.enable()) dialog.close();
	}

	function restoreFocus(): void {
		if (opener?.isConnected) {
			opener.focus();
			return;
		}
		focusFallback();
	}
</script>

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby="management-activation-title"
	onclose={restoreFocus}
	oncancel={(event) => {
		if (wizard.pending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-2xl management-activation-dialog">
		<p class="management-kicker"><ShieldAlert class="h-3.5 w-3.5" /> Write-access gate</p>
		<h2
			bind:this={heading}
			id="management-activation-title"
			tabindex="-1"
			class="font-display text-xl font-semibold"
		>
			Enable file organization
		</h2>
		<p class="mt-2 text-sm text-base-content/65">
			Nothing is enabled until a current dry run exists for every affected root and you confirm the
			exact phrase.
		</p>
		<div class="mt-5 flex flex-wrap gap-2" aria-label="Activation progress">
			{#each wizard.rootIds as rootId, index (rootId)}{@const root = roots.find(
					(value) => value.id === rootId
				)}<span
					class="management-activation-step"
					data-state={index < wizard.index
						? 'done'
						: index === wizard.index
							? 'current'
							: 'waiting'}
					>{#if index < wizard.index}<Check class="h-3.5 w-3.5" />{/if}{root?.label ?? rootId}</span
				>{/each}
		</div>
		{#if currentRoot}
			{@const activationProfile = wizard.profileFor(currentRoot.id)}
			<Card as="section" variant="bordered" padding="md" class="mt-5">
				<h3 class="font-semibold">Dry run · {currentRoot.label}</h3>
				<p class="mt-1 text-xs text-base-content/55">
					Previews the selected profile against this root. It reads files and writes only the saved
					preview.
				</p>
				{#if activationProfile}
					<Card variant="bordered" padding="sm" class="mt-3 text-xs">
						<strong>{activationProfile.name}</strong>
						<div class="mt-2 flex flex-wrap gap-1.5">
							{#each profileAspects(activationProfile) as aspect (aspect)}<span
									class="management-aspect">{aspect}</span
								>{/each}
						</div>
						<p class="mt-2 text-base-content/55">
							Organization remains inside this root. Only files with an accepted MusicBrainz release
							and per-track mapping are eligible.
						</p>
					</Card>
				{/if}
				{#if !wizard.jobId}
					<Button
						variant="warning"
						size="sm"
						class="mt-4 self-start"
						icon={RefreshCw}
						loading={wizard.starting}
						onclick={() => void wizard.run()}>Run dry run</Button
					>
				{:else if wizard.preview.isLoading}
					<div class="mt-4 skeleton h-20 rounded-xl"></div>
				{:else if wizard.preview.isError}
					<div class="alert alert-error mt-4 items-start text-sm" role="alert">
						<ShieldAlert class="mt-0.5 h-5 w-5" /><span
							><strong>Could not load this dry run.</strong><br />Retry the status request or run a
							fresh dry run before enabling anything.</span
						>
					</div>
					<div class="mt-3 flex flex-wrap gap-1">
						<Button
							variant="outline"
							size="sm"
							disabled={wizard.preview.isFetching}
							onclick={() => void wizard.preview.refetch()}>Retry status</Button
						>
						<Button
							variant="ghost"
							size="sm"
							disabled={wizard.pending}
							onclick={() => wizard.discard()}>Start a fresh dry run</Button
						>
					</div>
				{:else if preview}
					{#if !wizard.ready && (preview.summary.item_count ?? 0) > 0}<div
							class="mt-4 flex flex-wrap items-baseline justify-between gap-x-5 gap-y-1 rounded-lg border border-library-manage/20 bg-library-manage/5 px-3.5 py-3"
							aria-live="polite"
						>
							<span class="text-xs font-semibold uppercase tracking-[0.16em] text-base-content/45"
								>Planning progress</span
							>
							<span class="font-semibold text-base-content/80"
								>{(preview.summary.item_count ?? 0).toLocaleString()} files</span
							>
							<span class="text-sm text-base-content/55"
								>{(preview.summary.bundle_count ?? 0).toLocaleString()} release bundles</span
							>
						</div>{:else if !wizard.ready}<div
							class="mt-4 flex items-center gap-3 rounded-lg border border-library-manage/20 bg-library-manage/5 px-3.5 py-3 text-sm text-base-content/60"
							aria-live="polite"
						>
							<span
								class="loading loading-spinner loading-sm text-library-manage"
								aria-hidden="true"
							></span><span
								><strong class="block text-base-content/80">Discovering the root</strong>File and
								release totals appear after the first planning checkpoint.</span
							>
						</div>{/if}
					{#if wizard.ready}<div class="mt-4 grid grid-cols-2 gap-2 sm:grid-cols-4">
							<div class="management-mini-stat">
								<span>Eligible</span><strong>{preview.summary.eligible_count}</strong>
							</div>
							<div class="management-mini-stat">
								<span>Warnings</span><strong>{preview.summary.warning_count}</strong>
							</div>
							<div class="management-mini-stat">
								<span>Blocked</span><strong>{preview.summary.blocked_count}</strong>
							</div>
							<div class="management-mini-stat">
								<span>Moves</span><strong>{preview.summary.path_change_count}</strong>
							</div>
							<div class="management-mini-stat">
								<span>Tag writes</span><strong>{preview.summary.tag_change_count ?? 0}</strong>
							</div>
							<div class="management-mini-stat">
								<span>Artwork</span><strong>{preview.summary.artwork_change_count ?? 0}</strong>
							</div>
							<div class="management-mini-stat">
								<span>Sidecars</span><strong>{preview.summary.sidecar_change_count ?? 0}</strong>
							</div>
							<div class="management-mini-stat">
								<span>No change</span><strong>{preview.summary.no_change_count ?? 0}</strong>
							</div>
						</div>{/if}
					<Button
						variant="ghost"
						size="sm"
						class="mt-3 self-start"
						href={`/library/management/previews/${encodeURIComponent(wizard.jobId ?? '')}`}
						target="_blank"
						rel="noopener noreferrer">Review file-by-file dry run</Button
					>
					{#if preview.stale || preview.expired}<div class="alert alert-error mt-3 text-sm">
							This dry run is stale or expired. Run it again before enabling anything.
						</div>
						<Button
							variant="outline"
							size="sm"
							class="mt-3 self-start"
							onclick={() => wizard.discard()}>Run a fresh dry run</Button
						>
					{:else if ['failed', 'stopped', 'cancelled'].includes(preview.state)}<div
							class="alert alert-error mt-3 text-sm"
							role="alert"
						>
							{preview.state === 'failed'
								? 'This dry run failed during planning.'
								: 'This dry run was stopped.'} No files were changed.
						</div>
						<Button
							variant="outline"
							size="sm"
							class="mt-3 self-start"
							onclick={() => wizard.discard()}>Run a fresh dry run</Button
						>
					{:else if preview.control_request === 'stop'}<div
							class="mt-3 flex items-center gap-3 rounded-lg border border-warning/20 bg-warning/5 px-3 py-2.5 text-sm text-base-content/65"
							role="status"
							aria-live="polite"
						>
							<span class="loading loading-spinner loading-sm text-warning" aria-hidden="true"
							></span><span
								><strong class="block text-base-content/80">Stopping this dry run</strong>The
								planner will stop at its next safe checkpoint. No music files are involved.</span
							>
						</div>
					{:else if wizard.stalled}<div
							class="alert alert-error mt-3 items-start text-sm"
							role="alert"
						>
							<ShieldAlert class="mt-0.5 h-5 w-5 shrink-0" aria-hidden="true" /><span
								><strong class="block">Planning stopped responding</strong>The last saved checkpoint
								contains {(preview.summary.item_count ?? 0).toLocaleString()}
								files. No music files changed. Stop this dry run, then start a fresh one; planning resumes
								from a new operation.</span
							>
						</div>
					{:else if preview.state === 'queued'}<div
							class="mt-3 flex items-center gap-3 rounded-lg border border-base-content/10 bg-base-200/45 px-3 py-2.5 text-sm text-base-content/60"
							role="status"
							aria-live="polite"
						>
							<Clock3 class="h-5 w-5 shrink-0 text-library-manage" aria-hidden="true" /><span
								><strong class="block text-base-content/75">Queued for planning</strong>It starts
								automatically when the current library operation finishes. You can safely close this
								dialog.</span
							>
						</div>
					{:else if !wizard.ready}<div
							class="mt-3 flex items-center gap-3 rounded-lg border border-base-content/10 bg-base-200/45 px-3 py-2.5 text-sm text-base-content/60"
							role="status"
							aria-live="polite"
						>
							<span
								class="loading loading-spinner loading-sm text-library-manage"
								aria-hidden="true"
							></span><span
								><strong class="block text-base-content/75">Planning this dry run</strong>This
								updates automatically. You can safely close the dialog while it continues.</span
							>
						</div>{/if}
					<Button
						variant="warning"
						size="sm"
						class="mt-4 self-start"
						icon={ChevronRight}
						iconPosition="end"
						loading={wizard.accepting}
						disabled={!wizard.ready || wizard.pending}
						onclick={() => void wizard.accept()}>Use this dry run</Button
					>
				{/if}
			</Card>
		{:else}
			<section class="mt-5 space-y-3">
				<div class="alert alert-warning items-start">
					<ShieldAlert class="mt-0.5 h-5 w-5" /><span
						>Automatic management will write tags and may move files according to the previewed
						profiles. Closing this dialog leaves the saved configuration unchanged.</span
					>
				</div>
				<label class="grid gap-1.5 text-sm"
					><span>Type <strong>{LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}</strong></span><Input
						label={`Type ${LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}`}
						bind:value={wizard.phrase}
						autocomplete="off"
					/></label
				>
			</section>
		{/if}
		{#if wizard.error}<div class="alert alert-error mt-4 text-sm" role="alert">
				{wizard.error}
			</div>{/if}
		<div class="modal-action">
			{#if currentRoot && wizard.canStop}<Button
					variant="danger-ghost"
					class="mr-auto"
					icon={Square}
					loading={wizard.stopping}
					disabled={wizard.pending}
					onclick={() => void wizard.stopRun()}>Stop dry run</Button
				>
			{/if}
			<Button variant="ghost" onclick={() => dialog.close()} disabled={wizard.pending}>Close</Button
			>{#if !currentRoot}<Button
					variant="warning"
					loading={wizard.confirming}
					disabled={wizard.phrase !== LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE ||
						wizard.pending ||
						!wizard.coversDraft}
					onclick={() => void enable()}>Enable file organization</Button
				>{/if}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close organization activation" disabled={wizard.pending}>close</button>
	</form>
</dialog>

<style>
	.management-activation-dialog {
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 24%, transparent);
		background:
			linear-gradient(
				145deg,
				color-mix(in oklab, var(--color-library-manage) 5%, var(--color-base-100)),
				var(--color-base-100) 18rem
			),
			var(--color-base-100);
	}

	.management-activation-step {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		padding: 0.35rem 0.65rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 11%, transparent);
		border-radius: 9999px;
		color: color-mix(in oklab, var(--color-base-content) 50%, transparent);
		font-size: 0.7rem;
		font-weight: 700;
	}

	.management-activation-step[data-state='current'] {
		border-color: color-mix(in oklab, var(--color-library-manage) 45%, transparent);
		background: color-mix(in oklab, var(--color-library-manage) 11%, var(--color-base-100));
		color: var(--color-base-content);
	}

	.management-activation-step[data-state='done'] {
		border-color: color-mix(in oklab, var(--color-success) 35%, transparent);
		color: var(--color-success);
	}

	.management-mini-stat {
		display: grid;
		gap: 0.15rem;
		padding: 0.7rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 9%, transparent);
		border-radius: 0.65rem;
		background: var(--color-base-100);
	}

	.management-mini-stat span {
		color: color-mix(in oklab, var(--color-base-content) 48%, transparent);
		font-size: 0.65rem;
		text-transform: uppercase;
	}

	.management-mini-stat strong {
		font-family: var(--font-mono);
		font-size: 1.05rem;
	}
</style>
