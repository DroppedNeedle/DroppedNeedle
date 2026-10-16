<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import { TriangleAlert, FolderTree } from 'lucide-svelte';
	import { getLibraryPolicyTreeQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { getLibraryRunEstimateQuery } from '$lib/queries/library/LibraryOperationQueries.svelte';
	import { retryPendingLibraryReviews } from '$lib/queries/library/LibraryReviewMutations.svelte';
	import { getPendingReviewCountQuery } from '$lib/queries/library/LibraryReviewQueries.svelte';
	import type { ScanKind } from '$lib/queries/library/LibraryOperationsTypes';
	import { TransportError } from '$lib/api/client';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		open: boolean;
		kind: Extract<ScanKind, 'rescan_files' | 'policy_reconcile'> | 'retry_identification';
		onconfirm: (scopeIds: string[]) => void | Promise<void>;
		onclose: () => void;
		pending?: boolean;
	}

	let { open, kind, onconfirm, onclose, pending = false }: Props = $props();
	let dialog: HTMLDialogElement;
	let dialogHeading: HTMLHeadingElement;
	let opener: HTMLElement | null = null;
	let selected = $state<string[]>([]);
	let confirmLocalMetadata = $state(false);
	const treeQuery = getLibraryPolicyTreeQuery(() => open);
	const estimateQuery = getLibraryRunEstimateQuery(
		() => selected,
		() => open && kind !== 'retry_identification' && treeQuery.isSuccess
	);
	const retryAll = retryPendingLibraryReviews();
	const retryPreview = retryPendingLibraryReviews();
	const pendingReviews = getPendingReviewCountQuery();
	const retryIsTransport = $derived(retryAll.error instanceof TransportError);

	const roots = $derived(treeQuery.data?.roots ?? []);
	const selectedNodes = $derived.by(() => {
		if (!selected.length) return roots.flatMap((root) => [root, ...root.children]);
		return roots.flatMap((root) => {
			if (selected.includes(root.id)) return [root, ...root.children];
			return root.children.filter((rule) => selected.includes(rule.id));
		});
	});
	const hasUnavailable = $derived(selectedNodes.some((node) => !node.available));
	// Retry scope is whole folders: only selected roots count.
	const selectedRoots = $derived(selected.filter((id) => roots.some((root) => root.id === id)));
	// The preview counts exactly the chosen folders; before it, the whole
	// library's pending count is the best known.
	const pendingCount = $derived(
		retryPreview.data?.started ?? (selectedRoots.length ? null : (pendingReviews.data ?? 0))
	);
	const selectionLabel = $derived(
		selected.length === 0 ? 'Whole library' : `${selected.length} scopes`
	);
	const estimatedFiles = $derived(estimateQuery.data?.estimated_file_count);
	const title = $derived(
		kind === 'rescan_files'
			? 'Rescan files'
			: kind === 'retry_identification'
				? 'Retry identification'
				: 'Apply policy changes'
	);

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) {
			opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
			dialog.showModal();
			dialogHeading.focus();
		}
		if (!open && dialog.open) dialog.close();
	});

	function toggle(id: string): void {
		const node = roots
			.flatMap((root) => [root, ...root.children])
			.find((candidate) => candidate.id === id);
		if (node?.kind === 'rule') {
			selected = selected.includes(id) ? [] : [id];
		} else {
			const rootIds = new Set(roots.map((root) => root.id));
			const rootSelection = selected.filter((value) => rootIds.has(value));
			selected = rootSelection.includes(id)
				? rootSelection.filter((value) => value !== id)
				: [...rootSelection, id];
		}
	}

	function selectWholeLibrary(): void {
		selected = [];
	}

	// A changed scope or confirmation needs a fresh preview.
	$effect(() => {
		void selectedRoots.join();
		void confirmLocalMetadata;
		retryPreview.reset();
	});

	function close(): void {
		selected = [];
		retryAll.reset();
		retryPreview.reset();
		onclose();
		opener?.focus();
	}

	async function previewRetry(): Promise<void> {
		try {
			await retryPreview.mutateAsync({
				confirmLocalMetadata,
				rootIds: selectedRoots,
				preview: true
			});
		} catch {
			// mutation state keeps the error visible in this dialog
		}
	}

	async function startRetry(): Promise<void> {
		try {
			await retryAll.mutateAsync({ confirmLocalMetadata, rootIds: selectedRoots });
		} catch {
			// mutation state keeps the error visible in this dialog
		}
	}
</script>

<dialog bind:this={dialog} class="modal" onclose={close} aria-labelledby="library-work-title">
	<div class="modal-box max-w-2xl">
		<h2
			bind:this={dialogHeading}
			id="library-work-title"
			tabindex="-1"
			class="flex items-center gap-2 text-xl font-bold"
		>
			<FolderTree class="h-5 w-5 text-primary" aria-hidden="true" />
			{title}
		</h2>
		<p class="mt-2 text-sm text-base-content/65">
			{kind === 'rescan_files'
				? 'Read file tags again for the selected library scopes. Existing indexed music stays available while the scan runs.'
				: kind === 'retry_identification'
					? 'Look every album that waits for a decision up on MusicBrainz again, without rereading its files. Each one gets a fresh identification job.'
					: 'Reconcile the saved policies for the selected scopes. Work begins only after you confirm.'}
		</p>

		<div class="mt-5 space-y-2">
			<div class="flex items-center justify-between gap-3">
				<h3 class="font-semibold">Scope</h3>
				<Button variant="ghost" size="xs" onclick={selectWholeLibrary}>Whole library</Button>
			</div>
			{#if treeQuery.isLoading}
				<div class="skeleton h-28 rounded-box"></div>
			{:else if treeQuery.isError}
				<div class="alert alert-error text-sm">Could not load library scopes.</div>
			{:else}
				<div
					class="max-h-64 space-y-1 overflow-y-auto rounded-box border border-base-content/10 p-2"
				>
					{#each roots as root (root.id)}
						<label
							class="flex cursor-pointer items-center gap-3 rounded-lg px-2 py-2 hover:bg-base-200"
						>
							<input
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={selected.includes(root.id)}
								onchange={() => toggle(root.id)}
							/>
							<span class="min-w-0 flex-1">
								<span class="block truncate font-medium">{root.label}</span>
								<span class="block truncate text-xs text-base-content/55">{root.path}</span>
							</span>
							<span class="badge badge-outline badge-sm">{root.policy.replace('_', ' ')}</span>
							{#if !root.available}<span class="badge badge-warning badge-sm">Unavailable</span
								>{/if}
						</label>
						{#each kind === 'retry_identification' ? [] : root.children as rule (rule.id)}
							<label
								class="ml-6 flex cursor-pointer items-center gap-3 rounded-lg px-2 py-1.5 hover:bg-base-200"
							>
								<input
									type="checkbox"
									class="checkbox checkbox-sm"
									checked={selected.includes(rule.id)}
									onchange={() => toggle(rule.id)}
								/>
								<span class="min-w-0 flex-1 truncate text-sm">{rule.label}</span>
								<span class="badge badge-ghost badge-sm">{rule.policy.replace('_', ' ')}</span>
								{#if !rule.available}<span class="badge badge-warning badge-sm">Unavailable</span
									>{/if}
							</label>
						{/each}
					{/each}
				</div>
			{/if}
		</div>

		<div class="mt-4 rounded-box bg-base-200 p-3 text-sm">
			<div class="flex items-center justify-between gap-3">
				<span
					>{kind === 'retry_identification'
						? selectedRoots.length
							? `${selectedRoots.length} ${selectedRoots.length === 1 ? 'folder' : 'folders'}`
							: 'Whole library'
						: selectionLabel}</span
				>
				{#if kind === 'retry_identification' && (pendingReviews.isLoading || retryPreview.isPending)}
					<span class="skeleton h-4 w-28"></span>
				{:else if kind === 'retry_identification' && pendingCount === null}
					<span class="text-base-content/55">Preview to count albums</span>
				{:else if kind === 'retry_identification'}
					<strong
						>{(pendingCount ?? 0).toLocaleString()}
						{retryPreview.data
							? pendingCount === 1
								? 'album would start'
								: 'albums would start'
							: pendingCount === 1
								? 'album waits for a decision'
								: 'albums wait for a decision'}</strong
					>
				{:else if estimateQuery.isFetching}
					<span class="skeleton h-4 w-28"></span>
				{:else if estimatedFiles !== null && estimatedFiles !== undefined}
					<strong>About {estimatedFiles.toLocaleString()} files</strong>
				{:else}
					<span class="text-base-content/55">Count not available</span>
				{/if}
			</div>
			{#if estimateQuery.data?.estimated_at}
				<p class="mt-1 text-xs text-base-content/50">
					Approximate count from the latest inventory.
				</p>
			{/if}
		</div>

		{#if hasUnavailable && kind !== 'retry_identification'}
			<div class="alert alert-warning mt-4 text-sm">
				<TriangleAlert class="h-4 w-4" /> An unavailable scope cannot be processed until it is mounted.
			</div>
		{/if}
		{#if kind === 'retry_identification'}
			<label class="label mt-4 cursor-pointer justify-start gap-3 text-sm">
				<input type="checkbox" class="checkbox checkbox-sm" bind:checked={confirmLocalMetadata} />
				<span
					>Also look up albums in Local metadata folders. This is a one-off lookup; the folder's
					policy does not change.</span
				>
			</label>
			{#if retryPreview.data?.skipped.length && !retryAll.data}
				<ul class="mt-3 space-y-1 text-sm">
					{#each retryPreview.data.skipped as group (group.reason.code)}
						<li class="flex gap-2">
							<TriangleAlert class="mt-0.5 h-4 w-4 shrink-0 text-warning" />
							<span
								><strong>{group.count.toLocaleString()}</strong> would not start: {group.reason
									.message}
								<span class="text-base-content/60">{group.reason.action}</span></span
							>
						</li>
					{/each}
				</ul>
			{/if}
			{#if retryAll.isError || retryPreview.isError}
				<div class="alert alert-error mt-4 text-sm">
					{#if retryIsTransport}
						<span
							>DroppedNeedle could not reach the server, so the retry did not start. You can try
							again without closing this dialog.</span
						>
					{:else}
						<span>The retry could not start. No library data was changed.</span>
					{/if}
				</div>
			{/if}
			{#if retryAll.data}
				{@const result = retryAll.data}
				<div
					class="mt-4 rounded-box border border-primary/20 bg-base-100 p-4 text-sm"
					role="status"
				>
					<p>
						<strong>Started {result.started.toLocaleString()}</strong>
						{result.started === 1 ? 'identification job' : 'identification jobs'}. Each one shows up
						under Library operations and asks you to pick when it is done.
					</p>
					{#if result.skipped.length}
						<ul class="mt-2 space-y-1">
							{#each result.skipped as group (group.reason.code)}
								<li class="flex gap-2">
									<TriangleAlert class="mt-0.5 h-4 w-4 shrink-0 text-warning" />
									<span
										><strong>{group.count.toLocaleString()}</strong> did not start: {group.reason
											.message}
										<span class="text-base-content/60">{group.reason.action}</span></span
									>
								</li>
							{/each}
						</ul>
					{/if}
					<a
						class="link link-primary mt-2 inline-block"
						href={withBasePath('/library/review?state=pending')}>Open the reviews</a
					>
				</div>
			{/if}
		{/if}

		<div class="modal-action">
			<Button variant="ghost" onclick={() => dialog.close()}>Cancel</Button>
			{#if kind === 'retry_identification'}
				{#if !retryAll.data && !retryPreview.data}<Button
						variant="primary"
						loading={retryPreview.isPending}
						disabled={pendingCount === 0}
						onclick={() => void previewRetry()}>Preview retry</Button
					>{:else if !retryAll.data}<Button
						variant="primary"
						loading={retryAll.isPending}
						disabled={!retryPreview.data?.started}
						onclick={() => void startRetry()}
						>Retry {(retryPreview.data?.started ?? 0).toLocaleString()}
						{retryPreview.data?.started === 1 ? 'album' : 'albums'}</Button
					>{/if}
			{:else}<Button
					variant="primary"
					loading={pending}
					disabled={treeQuery.isLoading}
					onclick={() => void onconfirm(selected)}
				>
					{kind === 'rescan_files'
						? estimatedFiles
							? `Rescan about ${estimatedFiles.toLocaleString()} files`
							: 'Rescan selected files'
						: 'Apply policy changes'}
				</Button>{/if}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close dialog">close</button>
	</form>
</dialog>
