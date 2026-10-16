<script lang="ts">
	import { ChevronDown, ChevronRight, CircleAlert, Search } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';
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
	let sort = $state<'newest' | 'oldest' | 'tracks'>('newest');
	let withCandidates = $state(false);
	let retryDialog = $state<HTMLDialogElement>();
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
		withCandidates,
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

	function confirmation(review: LibraryReview, action: ReviewAction): string | null {
		if (action === 'exclude') {
			return `Take "${review.album_title}" out of the library? The files stay on disk, and you can put them back from this list.`;
		}
		if (action === 'detach') {
			return review.manual_identity
				? `"${review.album_title}" has an edition someone picked by hand. Remove that pick and its MusicBrainz match? The album keeps its own tags, and you can undo this from the album page.`
				: `Remove the MusicBrainz match from "${review.album_title}"? The album keeps its own tags, and you can undo this from the album page.`;
		}
		return null;
	}

	async function run(review: LibraryReview, action: ReviewAction): Promise<void> {
		const question = confirmation(review, action);
		if (question && !confirm(question)) return;
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

	async function previewRetry(): Promise<void> {
		try {
			await retryAll.mutateAsync({ confirmLocalMetadata, filter, preview: true });
			retryDialog?.showModal();
		} catch (error) {
			failure(error, 'Could not count what would start.');
		}
	}

	async function retryEverything(): Promise<void> {
		retryDialog?.close();
		try {
			const result = await retryAll.mutateAsync({ confirmLocalMetadata, filter });
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
		<Input
			type="search"
			size="sm"
			icon={Search}
			label="Search reviews"
			class="min-w-56 flex-1 sm:flex-none"
			placeholder="Album or artist"
			value={searchInput}
			oninput={(event) => onSearch(event.currentTarget.value)}
		/>

		{#if authStore.isAdmin && reviewState === 'pending' && total > 0}
			<div class="ml-auto flex items-center gap-3">
				<label class="label cursor-pointer gap-2 text-xs">
					<input type="checkbox" class="checkbox checkbox-sm" bind:checked={confirmLocalMetadata} />
					Include Local metadata folders
				</label>
				<Button size="sm" loading={retryAll.isPending} onclick={() => void previewRetry()}
					>Retry all {total} shown…</Button
				>
			</div>
		{/if}
	</div>

	<div class="flex flex-wrap items-end gap-2 text-sm">
		{#if authStore.isAdmin && roots.length > 1}
			<label class="grid gap-1 text-xs">
				<span>Folder</span>
				<select class="select select-sm" bind:value={rootId}>
					<option value="">All folders</option>
					{#each roots as root (root.id)}<option value={root.id}>{root.label}</option>{/each}
				</select>
			</label>
		{/if}
		<label class="grid gap-1 text-xs">
			<span>Folder policy</span>
			<select class="select select-sm" bind:value={policy}>
				<option value="">Any</option>
				<option value="automatic">Automatic</option>
				<option value="local_metadata">Local metadata</option>
				<option value="excluded">Excluded</option>
			</select>
		</label>
		<label class="grid gap-1 text-xs">
			<span>Identification</span>
			<select class="select select-sm" bind:value={jobState}>
				<option value="">Any</option>
				<option value="queued">Queued</option>
				<option value="running">Running</option>
				<option value="deferred">Waiting to retry</option>
				<option value="failed">Failed</option>
				<option value="attention">Needs attention</option>
				<option value="succeeded">Finished</option>
			</select>
		</label>
		<label class="grid gap-1 text-xs">
			<span>Filed from</span>
			<Input type="date" size="sm" label="Filed from" bind:value={filedFrom} />
		</label>
		<label class="grid gap-1 text-xs">
			<span>Filed to</span>
			<Input type="date" size="sm" label="Filed to" bind:value={filedTo} />
		</label>
		<label class="grid gap-1 text-xs">
			<span>Order</span>
			<select class="select select-sm" bind:value={sort}>
				<option value="newest">Newest first</option>
				<option value="oldest">Oldest first</option>
				<option value="tracks">Most tracks first</option>
			</select>
		</label>
		<label class="label cursor-pointer gap-2 pb-1 text-xs">
			<input type="checkbox" class="checkbox checkbox-sm" bind:checked={withCandidates} />
			Only with candidates
		</label>
		<label class="label cursor-pointer gap-2 pb-1 text-xs">
			<input type="checkbox" class="checkbox checkbox-sm" bind:checked={hideActiveJobs} />
			Hide albums still being matched
		</label>
	</div>

	{#if (query.data?.reason_counts.length ?? 0) > 1 || reasonCode}
		<div class="flex flex-wrap gap-2" role="group" aria-label="Reasons">
			<Button size="xs" variant={reasonCode ? 'ghost' : 'primary'} onclick={() => (reasonCode = '')}
				>All reasons</Button
			>
			{#each query.data?.reason_counts ?? [] as bucket (bucket.reason_code)}
				<Button
					size="xs"
					variant={reasonCode === bucket.reason_code ? 'primary' : 'ghost'}
					title={bucket.reason.action}
					onclick={() => (reasonCode = bucket.reason_code)}
					>{bucket.reason.message}
					<span class="badge badge-sm">{bucket.count}</span></Button
				>
			{/each}
		</div>
	{/if}

	{#if canSelect && selectionCount > 0}
		<div
			class="sticky top-2 z-10 flex flex-wrap items-center gap-2 rounded-box border border-primary/30 bg-base-100 p-2 text-sm shadow"
		>
			<strong class="mr-1">{selectionCount} selected</strong>
			{#if !allMatching && total > selected.length}
				<Button variant="ghost" size="xs" onclick={() => (allMatching = true)}
					>Select all {total} matching</Button
				>
			{/if}
			<span class="flex-1"></span>
			<Button size="sm" disabled={preview.isPending} onclick={() => void startBulk('keep_tagged')}
				>{BULK_LABEL.keep_tagged}</Button
			>
			<Button size="sm" disabled={preview.isPending} onclick={() => void startBulk('accept')}
				>{BULK_LABEL.accept}</Button
			>
			{#if authStore.isAdmin}
				<Button
					variant="danger-ghost"
					size="sm"
					disabled={preview.isPending}
					onclick={() => void startBulk('exclude')}>{BULK_LABEL.exclude}</Button
				>
			{/if}
			<Button
				variant="ghost"
				size="sm"
				onclick={() => {
					selected = [];
					allMatching = false;
				}}>Clear</Button
			>
		</div>
	{/if}

	{#if retryAll.data && !retryAll.data.preview && retryAll.data.skipped.length}
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
			<Button size="sm" onclick={() => query.refetch()}>Retry</Button>
		</div>
	{:else if !reviews.length}
		<Card class="text-center text-base-content/60">
			{reviewState === 'pending'
				? 'Nothing is waiting on you.'
				: reviewState === 'excluded'
					? 'No album was taken out of the library from a review.'
					: 'No reviews yet.'}
		</Card>
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
							<Button
								variant="ghost"
								size="sm"
								href={withBasePath(`/album/${encodeURIComponent(review.album_id)}`)}
								>Open album</Button
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
												{#if candidate.track_evidence.length}
													<details class="mt-1 text-xs">
														<summary class="cursor-pointer text-base-content/60"
															>How each file fits</summary
														>
														<ul class="mt-1 space-y-0.5">
															{#each candidate.track_evidence as evidence (evidence.local_track_id)}
																<li class="flex items-center gap-2">
																	<span
																		class="badge badge-xs {evidence.classification === 'supported'
																			? 'badge-success'
																			: evidence.classification === 'contradictory'
																				? 'badge-error'
																				: 'badge-ghost'}">{evidence.classification}</span
																	>
																	<span class="truncate"
																		>{evidence.title || evidence.local_track_id}</span
																	>
																	{#if evidence.evidence_kinds.length}<span
																			class="text-base-content/50"
																			>({evidence.evidence_kinds.join(', ')})</span
																		>{/if}
																</li>
															{/each}
														</ul>
													</details>
												{/if}
											</div>
											{#if authStore.isTrusted && review.state === 'pending'}
												<Button
													variant="primary"
													size="xs"
													disabled={busy}
													onclick={() => void choose(review, candidate.candidate_key)}
													>Use this release</Button
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
										<Button
											size="sm"
											disabled={busy}
											onclick={() => void run(review, 'keep_tagged')}>Keep as tagged</Button
										>
										<Button
											variant="ghost"
											size="sm"
											disabled={busy}
											title="Take the MusicBrainz match off; the album keeps its own tags"
											onclick={() => void run(review, 'detach')}>Remove match</Button
										>
										<Button
											variant="ghost"
											size="sm"
											disabled={busy}
											onclick={() => void run(review, 'dismiss')}>Close without deciding</Button
										>
									{/if}
									{#if authStore.isAdmin}
										<Button
											variant="ghost"
											size="sm"
											disabled={busy}
											onclick={() => void retryOne(review)}>Retry identification</Button
										>
										<Button
											variant="danger-ghost"
											size="sm"
											disabled={busy}
											onclick={() => void run(review, 'exclude')}>Take out of library</Button
										>
									{/if}
								{:else if review.state === 'excluded' && authStore.isAdmin}
									<Button size="sm" disabled={busy} onclick={() => void run(review, 'restore')}
										>Put back in library</Button
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
					<Button
						size="sm"
						class="join-item"
						disabled={offset === 0}
						onclick={() => (offset = Math.max(0, offset - PAGE))}>Previous</Button
					>
					<Button
						size="sm"
						class="join-item"
						disabled={offset + PAGE >= total}
						onclick={() => (offset += PAGE)}>Next</Button
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
				{#if bulkSelection?.action === 'accept'}Each album gets its best-scoring release, and that
					edition is pinned: automatic matching leaves these albums alone afterwards. You can undo
					each one from its album page.{/if}
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
			<Button variant="ghost" onclick={() => bulkDialog?.close()}>Close</Button>
			{#if !applyBulk.data}
				<Button
					variant="primary"
					loading={applyBulk.isPending}
					disabled={!preview.data?.eligible}
					onclick={() => void confirmBulk()}>Apply to {preview.data?.eligible ?? 0}</Button
				>
			{/if}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close dialog">close</button>
	</form>
</dialog>

<dialog bind:this={retryDialog} class="modal" aria-labelledby="retry-review-title">
	<div class="modal-box max-w-lg">
		<h2 id="retry-review-title" class="text-lg font-bold">Retry identification</h2>
		{#if retryAll.data?.preview}
			{@const plan = retryAll.data}
			<p class="mt-2 text-sm">
				<strong>{plan.started}</strong>
				{plan.started === 1 ? 'album' : 'albums'} would be looked up on MusicBrainz again. Each gets its
				own identification job, and its review closes.
			</p>
			{#if plan.skipped.length}
				<ul class="mt-3 space-y-1 text-sm">
					{#each plan.skipped as group (group.reason.code)}
						<li>
							<strong>{group.count}</strong> would not start: {group.reason.message}
							<span class="text-base-content/60">{group.reason.action}</span>
						</li>
					{/each}
				</ul>
			{/if}
		{/if}
		<div class="modal-action">
			<Button variant="ghost" onclick={() => retryDialog?.close()}>Cancel</Button>
			<Button
				variant="primary"
				disabled={!retryAll.data?.preview || !retryAll.data.started}
				onclick={() => void retryEverything()}>Retry {retryAll.data?.started ?? 0}</Button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close dialog">close</button>
	</form>
</dialog>
