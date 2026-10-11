<script lang="ts">
	import { ChevronDown, ChevronRight, CircleAlert, Search } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import type {
		LibraryReviewParams,
		LibraryReviewState
	} from '$lib/queries/library/LibraryQueryKeyFactory';
	import { getLibraryPolicyTreeQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import {
		getLibraryReviewQuery,
		getLibraryReviewsQuery,
		type LibraryReview
	} from '$lib/queries/library/LibraryReviewQueries.svelte';
	import {
		acceptLibraryReviewCandidate,
		actOnLibraryReview,
		retryLibraryReview,
		retryPendingLibraryReviews,
		previewBulkLibraryReviews,
		applyBulkLibraryReviews,
		type BulkReviewAction,
		type BulkReviewSelection,
		type ReviewAction
	} from '$lib/queries/library/LibraryReviewMutations.svelte';

	interface Props {
		/** Open this review first (repair findings link straight to one). */
		focusReviewId?: string | null;
		initialState?: LibraryReviewState;
		initialReason?: string;
	}

	let { focusReviewId = null, initialState = 'pending', initialReason = '' }: Props = $props();

	const PAGE = 25;
	// svelte-ignore state_referenced_locally
	let reviewState = $state<LibraryReviewState>(initialState);
	// svelte-ignore state_referenced_locally
	let reasonCode = $state(initialReason);
	let searchInput = $state('');
	let search = $state('');
	let offset = $state(0);
	// svelte-ignore state_referenced_locally
	let expanded = $state<string | null>(focusReviewId);
	let confirmLocalMetadata = $state(false);
	let searchTimer: ReturnType<typeof setTimeout> | undefined;
	let rootId = $state('');
	let policy = $state<'' | NonNullable<LibraryReviewParams['policy']>>('');
	let jobState = $state<'' | NonNullable<LibraryReviewParams['jobState']>>('');
	let hideActiveJobs = $state(false);
	let filedFrom = $state('');
	let filedTo = $state('');
	let sort = $state<'newest' | 'oldest'>('newest');
	let selected = $state<string[]>([]);
	let allMatching = $state(false);
	let bulkDialog = $state<HTMLDialogElement>();
	let bulkSelection = $state<BulkReviewSelection | null>(null);

	function dayStart(value: string): number | undefined {
		const time = value ? new Date(`${value}T00:00:00`).getTime() : NaN;
		return Number.isFinite(time) ? time : undefined;
	}

	const filter = $derived<Omit<LibraryReviewParams, 'limit' | 'offset'>>({
		state: reviewState,
		search: search || undefined,
		reasonCode: reasonCode || undefined,
		rootId: rootId || undefined,
		policy: policy || undefined,
		jobState: jobState || undefined,
		hideActiveJobs,
		createdFrom: dayStart(filedFrom),
		createdTo: filedTo ? (dayStart(filedTo) ?? 0) + 86_399_999 : undefined,
		sort
	});
	const query = getLibraryReviewsQuery(() => ({ ...filter, limit: PAGE, offset }));
	const tree = getLibraryPolicyTreeQuery(() => authStore.isAdmin);
	const roots = $derived(tree.data?.roots ?? []);
	const preview = previewBulkLibraryReviews();
	const applyBulk = applyBulkLibraryReviews();
	const canSelect = $derived(authStore.isTrusted && reviewState === 'pending');
	const selectionCount = $derived(allMatching ? (query.data?.total ?? 0) : selected.length);

	// A changed filter starts the page and the selection over.
	$effect(() => {
		void JSON.stringify(filter);
		offset = 0;
		selected = [];
		allMatching = false;
	});

	function toggle(id: string): void {
		allMatching = false;
		selected = selected.includes(id) ? selected.filter((value) => value !== id) : [...selected, id];
	}

	async function startBulk(action: BulkReviewAction): Promise<void> {
		const selection: BulkReviewSelection = {
			action,
			reviewIds: allMatching ? [] : selected,
			filter
		};
		bulkSelection = selection;
		applyBulk.reset();
		try {
			await preview.mutateAsync(selection);
			bulkDialog?.showModal();
		} catch (error) {
			failure(error, 'Could not preview the change.');
		}
	}

	async function confirmBulk(): Promise<void> {
		if (!bulkSelection || !preview.data) return;
		try {
			const result = await applyBulk.mutateAsync({
				selection: bulkSelection,
				previewToken: preview.data.preview_token
			});
			toastStore.show({
				message: `Changed ${result.applied} ${result.applied === 1 ? 'review' : 'reviews'}.`,
				type: 'success'
			});
			selected = [];
			allMatching = false;
			if (!result.skipped.length) bulkDialog?.close();
		} catch (error) {
			failure(error, 'Could not apply the change.');
		}
	}

	const BULK_LABEL: Record<BulkReviewAction, string> = {
		keep_tagged: 'Keep as tagged',
		accept: 'Use the best release',
		exclude: 'Take out of library'
	};
	const focused = getLibraryReviewQuery(() => focusReviewId);
	const act = actOnLibraryReview();
	const accept = acceptLibraryReviewCandidate();
	const retry = retryLibraryReview();
	const retryAll = retryPendingLibraryReviews();

	const reviews = $derived.by(() => {
		const page = query.data?.reviews ?? [];
		const pinned = focused.data;
		if (!pinned || offset > 0 || page.some((review) => review.id === pinned.id)) return page;
		return [pinned, ...page];
	});
	const total = $derived(query.data?.total ?? 0);
	const busy = $derived(act.isPending || accept.isPending || retry.isPending);

	function setState(next: LibraryReviewState): void {
		reviewState = next;
	}

	function onSearch(value: string): void {
		searchInput = value;
		clearTimeout(searchTimer);
		searchTimer = setTimeout(() => {
			search = value.trim();
		}, 300);
	}

	function failure(error: unknown, fallback: string): void {
		const message = error instanceof ApiError || error instanceof Error ? error.message : fallback;
		toastStore.show({ message, type: 'error' });
	}

	async function run(review: LibraryReview, action: ReviewAction): Promise<void> {
		if (
			action === 'exclude' &&
			!confirm(
				`Take "${review.album_title}" out of the library? The files stay on disk, and you can put them back from this list.`
			)
		) {
			return;
		}
		try {
			await act.mutateAsync({ reviewId: review.id, action });
		} catch (error) {
			failure(error, 'Could not save the decision.');
		}
	}

	async function choose(review: LibraryReview, candidateKey: string): Promise<void> {
		try {
			await accept.mutateAsync({ reviewId: review.id, candidateKey });
		} catch (error) {
			failure(error, 'Could not use this release.');
		}
	}

	async function retryOne(review: LibraryReview): Promise<void> {
		try {
			await retry.mutateAsync({ reviewId: review.id, confirmLocalMetadata });
		} catch (error) {
			failure(error, 'Could not start identification again.');
		}
	}

	async function retryEverything(): Promise<void> {
		try {
			const result = await retryAll.mutateAsync({
				confirmLocalMetadata,
				rootIds: rootId ? [rootId] : []
			});
			const held = result.skipped.reduce((sum, group) => sum + group.count, 0);
			toastStore.show({
				message: held
					? `Started ${result.started}; ${held} could not start (see below).`
					: `Started identification again for ${result.started} ${result.started === 1 ? 'album' : 'albums'}.`,
				type: held ? 'info' : 'success'
			});
		} catch (error) {
			failure(error, 'Could not start identification again.');
		}
	}

	function percent(score: number): string {
		return `${Math.round(Math.max(0, Math.min(1, score)) * 100)}%`;
	}
</script>

<div class="space-y-4">
	<p class="max-w-3xl text-sm text-base-content/65">
		A few cases still wait on a person: an undone automatic edition with nothing to go back to,
		editions dropped when albums were combined, and repair findings. Pick a release, keep the album
		as tagged, or close the review.
	</p>

	<div class="flex flex-wrap items-center gap-2">
		<div role="tablist" class="tabs tabs-boxed w-fit">
			{#each [['pending', 'Waiting'], ['excluded', 'Taken out'], ['all', 'Everything']] as [value, label] (value)}
				<button
					role="tab"
					class="tab {reviewState === value ? 'tab-active' : ''}"
					aria-selected={reviewState === value}
					onclick={() => setState(value as LibraryReviewState)}>{label}</button
				>
			{/each}
		</div>
		<label
			class="input input-sm input-bordered flex min-w-56 flex-1 items-center gap-2 sm:flex-none"
		>
			<Search class="h-4 w-4 opacity-60" />
			<input
				type="search"
				class="grow"
				placeholder="Album or artist"
				value={searchInput}
				oninput={(event) => onSearch(event.currentTarget.value)}
			/>
		</label>
		{#if reasonCode}
			<button class="btn btn-ghost btn-sm" onclick={() => (reasonCode = '')}
				>Showing one reason · show all</button
			>
		{/if}
		{#if authStore.isAdmin && reviewState === 'pending' && total > 0}
			<div class="ml-auto flex items-center gap-3">
				<label class="label cursor-pointer gap-2 text-xs">
					<input type="checkbox" class="checkbox checkbox-xs" bind:checked={confirmLocalMetadata} />
					Include Local metadata folders
				</label>
				<button
					class="btn btn-sm"
					disabled={retryAll.isPending}
					onclick={() => void retryEverything()}
					>{#if retryAll.isPending}<span class="loading loading-spinner loading-xs"></span>{/if}
					Retry all{rootId ? ' in this folder' : ''}</button
				>
			</div>
		{/if}
	</div>

	<div class="flex flex-wrap items-end gap-2 text-sm">
		{#if authStore.isAdmin && roots.length > 1}
			<label class="form-control">
				<span class="label-text text-xs">Folder</span>
				<select class="select select-bordered select-sm" bind:value={rootId}>
					<option value="">All folders</option>
					{#each roots as root (root.id)}<option value={root.id}>{root.label}</option>{/each}
				</select>
			</label>
		{/if}
		<label class="form-control">
			<span class="label-text text-xs">Folder policy</span>
			<select class="select select-bordered select-sm" bind:value={policy}>
				<option value="">Any</option>
				<option value="automatic">Automatic</option>
				<option value="local_metadata">Local metadata</option>
				<option value="excluded">Excluded</option>
			</select>
		</label>
		<label class="form-control">
			<span class="label-text text-xs">Identification</span>
			<select class="select select-bordered select-sm" bind:value={jobState}>
				<option value="">Any</option>
				<option value="queued">Queued</option>
				<option value="running">Running</option>
				<option value="deferred">Waiting to retry</option>
				<option value="failed">Failed</option>
				<option value="attention">Needs attention</option>
				<option value="succeeded">Finished</option>
			</select>
		</label>
		<label class="form-control">
			<span class="label-text text-xs">Filed from</span>
			<input type="date" class="input input-bordered input-sm" bind:value={filedFrom} />
		</label>
		<label class="form-control">
			<span class="label-text text-xs">Filed to</span>
			<input type="date" class="input input-bordered input-sm" bind:value={filedTo} />
		</label>
		<label class="form-control">
			<span class="label-text text-xs">Order</span>
			<select class="select select-bordered select-sm" bind:value={sort}>
				<option value="newest">Newest first</option>
				<option value="oldest">Oldest first</option>
			</select>
		</label>
		<label class="label cursor-pointer gap-2 pb-1 text-xs">
			<input type="checkbox" class="checkbox checkbox-xs" bind:checked={hideActiveJobs} />
			Hide albums still being matched
		</label>
	</div>

	{#if canSelect && selectionCount > 0}
		<div
			class="sticky top-2 z-10 flex flex-wrap items-center gap-2 rounded-box border border-primary/30 bg-base-100 p-2 text-sm shadow"
		>
			<strong class="mr-1">{selectionCount} selected</strong>
			{#if !allMatching && total > selected.length}
				<button class="btn btn-ghost btn-xs" onclick={() => (allMatching = true)}
					>Select all {total} matching</button
				>
			{/if}
			<span class="flex-1"></span>
			<button
				class="btn btn-sm"
				disabled={preview.isPending}
				onclick={() => void startBulk('keep_tagged')}>{BULK_LABEL.keep_tagged}</button
			>
			<button
				class="btn btn-sm"
				disabled={preview.isPending}
				onclick={() => void startBulk('accept')}>{BULK_LABEL.accept}</button
			>
			{#if authStore.isAdmin}
				<button
					class="btn btn-ghost btn-sm text-error"
					disabled={preview.isPending}
					onclick={() => void startBulk('exclude')}>{BULK_LABEL.exclude}</button
				>
			{/if}
			<button
				class="btn btn-ghost btn-sm"
				onclick={() => {
					selected = [];
					allMatching = false;
				}}>Clear</button
			>
		</div>
	{/if}

	{#if retryAll.data?.skipped.length}
		<div class="alert alert-warning text-sm">
			<CircleAlert class="h-4 w-4" />
			<ul class="space-y-1">
				{#each retryAll.data.skipped as group (group.reason.code)}
					<li>
						<strong>{group.count}</strong>: {group.reason.message}
						<span class="text-base-content/70">{group.reason.action}</span>
					</li>
				{/each}
			</ul>
		</div>
	{/if}

	{#if query.isLoading}
		<div class="space-y-2">
			{#each Array(4) as _, index (index)}<div class="skeleton h-20 w-full"></div>{/each}
		</div>
	{:else if query.isError}
		<div class="alert alert-error">
			<span>The reviews couldn't be loaded.</span>
			<button class="btn btn-sm" onclick={() => query.refetch()}>Retry</button>
		</div>
	{:else if !reviews.length}
		<div class="rounded-box bg-base-200 p-8 text-center text-base-content/60">
			{reviewState === 'pending'
				? 'Nothing is waiting on you.'
				: reviewState === 'excluded'
					? 'No album was taken out of the library from a review.'
					: 'No reviews yet.'}
		</div>
	{:else}
		<ul class="space-y-2">
			{#each reviews as review (review.id)}
				{@const open = expanded === review.id}
				<li class="rounded-box border border-base-content/10 bg-base-100">
					<div class="flex flex-wrap items-start justify-between gap-3 p-3">
						{#if canSelect}
							<input
								type="checkbox"
								class="checkbox checkbox-sm mt-0.5"
								aria-label={`Select ${review.album_title}`}
								checked={allMatching || selected.includes(review.id)}
								disabled={allMatching}
								onchange={() => toggle(review.id)}
							/>
						{/if}
						<button
							class="flex min-w-0 flex-1 items-start gap-2 text-left"
							aria-expanded={open}
							onclick={() => (expanded = open ? null : review.id)}
						>
							{#if open}<ChevronDown class="mt-0.5 h-4 w-4 shrink-0" />{:else}<ChevronRight
									class="mt-0.5 h-4 w-4 shrink-0"
								/>{/if}
							<span class="min-w-0">
								<span class="block truncate font-medium"
									>{review.album_title || 'Unknown album'}</span
								>
								<span class="block truncate text-xs text-base-content/55">{review.artist_name}</span
								>
								<span class="mt-1 block text-sm text-base-content/70">{review.reason.message}</span>
								<span class="block text-xs text-base-content/55">{review.reason.action}</span>
							</span>
						</button>
						<div class="flex flex-wrap items-center gap-2">
							{#if review.state !== 'pending'}
								<span class="badge badge-ghost badge-sm capitalize">{review.state}</span>
							{/if}
							<a
								class="btn btn-ghost btn-sm"
								href={withBasePath(`/album/${encodeURIComponent(review.album_id)}`)}>Open album</a
							>
						</div>
					</div>

					{#if open}
						<div class="space-y-3 border-t border-base-content/10 p-3">
							{#if review.candidates.length}
								<ul class="space-y-2">
									{#each review.candidates as candidate (candidate.candidate_key)}
										<li
											class="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-base-200 px-3 py-2"
										>
											<div class="min-w-0">
												<p class="truncate text-sm font-medium">{candidate.album_title}</p>
												<p class="truncate text-xs text-base-content/60">
													{candidate.album_artist_name}
													{#if candidate.score > 0}· {percent(candidate.score)} fit{/if}
													{#if candidate.supported_tracks}· {candidate.supported_tracks} tracks agree{/if}
													{#if candidate.contradictory_tracks}· {candidate.contradictory_tracks} disagree{/if}
												</p>
											</div>
											{#if authStore.isTrusted && review.state === 'pending'}
												<button
													class="btn btn-primary btn-xs"
													disabled={busy}
													onclick={() => void choose(review, candidate.candidate_key)}
													>Use this release</button
												>
											{/if}
										</li>
									{/each}
								</ul>
							{:else}
								<p class="text-sm text-base-content/60">
									No candidates were kept with this review. Retry identification, or pick an edition
									from the album page.
								</p>
							{/if}

							<div class="flex flex-wrap gap-2">
								{#if review.state === 'pending'}
									{#if authStore.isTrusted}
										<button
											class="btn btn-sm"
											disabled={busy}
											onclick={() => void run(review, 'keep_tagged')}>Keep as tagged</button
										>
										<button
											class="btn btn-ghost btn-sm"
											disabled={busy}
											title="Take the MusicBrainz match off; the album keeps its own tags"
											onclick={() => void run(review, 'detach')}>Remove match</button
										>
										<button
											class="btn btn-ghost btn-sm"
											disabled={busy}
											onclick={() => void run(review, 'dismiss')}>Close without deciding</button
										>
									{/if}
									{#if authStore.isAdmin}
										<button
											class="btn btn-ghost btn-sm"
											disabled={busy}
											onclick={() => void retryOne(review)}>Retry identification</button
										>
										<button
											class="btn btn-ghost btn-sm text-error"
											disabled={busy}
											onclick={() => void run(review, 'exclude')}>Take out of library</button
										>
									{/if}
								{:else if review.state === 'excluded' && authStore.isAdmin}
									<button
										class="btn btn-sm"
										disabled={busy}
										onclick={() => void run(review, 'restore')}>Put back in library</button
									>
								{/if}
							</div>
						</div>
					{/if}
				</li>
			{/each}
		</ul>
		{#if total > PAGE}
			<div class="flex items-center justify-between text-sm">
				<span class="text-base-content/60"
					>{offset + 1}–{Math.min(offset + PAGE, total)} of {total}</span
				>
				<div class="join">
					<button
						class="btn btn-sm join-item"
						disabled={offset === 0}
						onclick={() => (offset = Math.max(0, offset - PAGE))}>Previous</button
					>
					<button
						class="btn btn-sm join-item"
						disabled={offset + PAGE >= total}
						onclick={() => (offset += PAGE)}>Next</button
					>
				</div>
			</div>
		{/if}
	{/if}
</div>

<dialog bind:this={bulkDialog} class="modal" aria-labelledby="bulk-review-title">
	<div class="modal-box max-w-lg">
		<h2 id="bulk-review-title" class="text-lg font-bold">
			{bulkSelection ? BULK_LABEL[bulkSelection.action] : ''}
		</h2>
		{#if preview.data}
			{@const plan = preview.data}
			<p class="mt-2 text-sm">
				This changes <strong>{plan.eligible}</strong> of the {plan.matched} selected
				{plan.matched === 1 ? 'review' : 'reviews'}.
				{#if bulkSelection?.action === 'accept'}Each album gets its best-scoring release.{/if}
				{#if bulkSelection?.action === 'exclude'}Their files stay on disk and can be put back from
					the Taken out list.{/if}
			</p>
			{#if plan.skipped.length}
				<ul class="mt-3 space-y-1 text-sm">
					{#each plan.skipped as group (group.reason.code)}
						<li>
							<strong>{group.count}</strong> left alone: {group.reason.message}
							<span class="text-base-content/60">{group.reason.action}</span>
						</li>
					{/each}
				</ul>
			{/if}
			{#if applyBulk.data}
				<div class="alert alert-success mt-3 text-sm" role="status">
					Changed {applyBulk.data.applied}.
					{#each applyBulk.data.skipped as group (group.reason.code)}
						{group.count} left alone: {group.reason.message}
					{/each}
				</div>
			{/if}
		{/if}
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => bulkDialog?.close()}>Close</button>
			{#if !applyBulk.data}
				<button
					class="btn btn-primary"
					disabled={applyBulk.isPending || !preview.data?.eligible}
					onclick={() => void confirmBulk()}
					>{#if applyBulk.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
					Apply to {preview.data?.eligible ?? 0}</button
				>
			{/if}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close dialog">close</button>
	</form>
</dialog>
