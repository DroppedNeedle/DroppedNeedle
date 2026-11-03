<!--
	The identification review list: five views with counts, filters kept in
	the page address, the reviews (grouped when ordered by reason, with
	per-reason actions), and bulk changes that are previewed before they
	apply. A bulk change or retry runs as a background job, shown above the
	list with its progress and pause, resume and stop.
-->
<script lang="ts">
	import { tick, untrack } from 'svelte';
	import { CircleAlert, Search, SlidersHorizontal } from 'lucide-svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import Button from '$lib/ui/Button.svelte';
	import Tabs from '$lib/ui/Tabs.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import Input from '$lib/ui/Input.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import OperationJobStrip from './OperationJobStrip.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type {
		LibraryReviewParams,
		LibraryReviewState
	} from '$lib/queries/library/LibraryQueryKeyFactory';
	import { getLibraryPolicyTreeQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import {
		getLibraryReviewJobsQuery,
		getLibraryReviewQuery,
		getLibraryReviewsQuery,
		type LibraryReview,
		type LibraryReviewJob,
		type LibraryReviewPage
	} from '$lib/queries/library/LibraryReviewQueries.svelte';
	import {
		createAcceptLibraryReviewCandidateMutation,
		createActOnLibraryReviewMutation,
		createRetryLibraryReviewMutation,
		createRetryPendingLibraryReviewsMutation,
		createPreviewBulkLibraryReviewsMutation,
		createApplyBulkLibraryReviewsMutation,
		createControlLibraryReviewJobMutation,
		reviewJobEnded,
		type BulkReviewAction,
		type BulkReviewResult,
		type BulkReviewSelection,
		type ReviewAction,
		type ReviewJobControl
	} from '$lib/queries/library/LibraryReviewMutations.svelte';
	import ReviewFilterBar from './review/ReviewFilterBar.svelte';
	import ReviewItem from './review/ReviewItem.svelte';
	import ReviewSharedRelease from './review/ReviewSharedRelease.svelte';
	import { REVIEW_VIEWS, ReviewFilters } from './review/reviewFilters.svelte';
	import { failureSentence } from './edition/editionCopy';

	const PAGE = 25;
	const focusReviewId = page.url.searchParams.get('review');
	const filters = new ReviewFilters(page.url.searchParams);
	let searchInput = $state(filters.search);
	let searchTimer: ReturnType<typeof setTimeout> | undefined;
	$effect(() => () => clearTimeout(searchTimer));
	let expanded = $state<string | null>(focusReviewId);
	let confirmLocalMetadata = $state(false);
	let selected = $state<string[]>([]);
	let allMatching = $state(false);

	const filter = $derived(filters.params);

	// The address follows the filters, so a link or a reload reopens this view.
	// Only a filter change writes it: leaving the page must not.
	$effect(() => {
		const current = untrack(() => page.url);
		const url = new URL(current);
		filters.writeTo(url);
		if (url.href !== current.href) {
			void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
		}
	});

	// A changed filter starts the selection over.
	$effect(() => {
		void JSON.stringify(filter);
		selected = [];
		allMatching = false;
	});

	const query = getLibraryReviewsQuery(() => ({ ...filter, limit: PAGE }));
	const firstPage = $derived(query.data?.pages[0]);
	const tree = getLibraryPolicyTreeQuery(() => authStore.isAdmin);
	const roots = $derived(tree.data?.roots ?? []);
	const focused = getLibraryReviewQuery(() => focusReviewId);
	const act = createActOnLibraryReviewMutation();
	const accept = createAcceptLibraryReviewCandidateMutation();
	const retry = createRetryLibraryReviewMutation();
	const retryAll = createRetryPendingLibraryReviewsMutation();
	const preview = createPreviewBulkLibraryReviewsMutation();
	const applyBulk = createApplyBulkLibraryReviewsMutation();

	const pendingView = $derived(filters.state === 'pending');
	const canSelect = $derived(authStore.isTrusted && pendingView);
	const total = $derived(firstPage?.total ?? 0);
	const selectionCount = $derived(allMatching ? total : selected.length);
	const busy = $derived(act.isPending || accept.isPending || retry.isPending);

	const reviews = $derived.by(() => {
		const loaded = query.data?.pages.flatMap((page) => page.items) ?? [];
		const pinned = focused.data;
		if (!pinned || loaded.some((review) => review.id === pinned.id)) return loaded;
		return [pinned, ...loaded];
	});

	function viewCount(state: LibraryReviewState, counts: LibraryReviewPage['state_counts']): number {
		switch (state) {
			case 'resolved':
				return counts.approved + counts.dismissed;
			case 'all':
				return (
					counts.pending + counts.approved + counts.rejected + counts.dismissed + counts.excluded
				);
			default:
				return counts[state];
		}
	}

	const EMPTY: Record<LibraryReviewState, string> = {
		pending: 'Nothing needs you.',
		rejected: 'No review had its tags kept.',
		excluded: 'No album was taken out of the library from a review.',
		resolved: 'No review was resolved yet.',
		approved: 'No review was resolved yet.',
		dismissed: 'No review was closed yet.',
		all: 'No reviews yet.'
	};

	function onSearch(value: string): void {
		searchInput = value;
		clearTimeout(searchTimer);
		searchTimer = setTimeout(() => {
			filters.search = value.trim();
		}, 300);
	}

	function failure(error: unknown, fallback: string): void {
		toastStore.show({ message: failureSentence(error, fallback), type: 'error' });
	}

	// After acting on a review, focus moves on to the next one.
	let list = $state<HTMLUListElement | null>(null);
	async function focusAfter(reviewId: string): Promise<void> {
		const at = reviews.findIndex((review) => review.id === reviewId);
		const next = reviews[at + 1]?.id;
		await tick();
		if (!next) return;
		list
			?.querySelector<HTMLElement>(`[data-review="${CSS.escape(next)}"] [data-row-focus]`)
			?.focus();
	}

	let filtersOpen = $state(false);

	function plural(count: number, word: string): string {
		return `${count} ${count === 1 ? word : `${word}s`}`;
	}

	// One review's actions. Excluding and removing the match ask first.
	let asking = $state<{ review: LibraryReview; action: 'exclude' | 'detach' } | null>(null);
	let askOpen = $state(false);

	async function run(review: LibraryReview, action: ReviewAction): Promise<void> {
		if (action === 'exclude' || action === 'detach') {
			asking = { review, action };
			askOpen = true;
			return;
		}
		try {
			const settled = await act.mutateAsync({
				reviewId: review.id,
				revision: review.row_revision,
				action,
				title: review.album_title
			});
			if (action === 'keep_tagged' || action === 'dismiss') offerReopen(settled, action);
			void focusAfter(review.id);
		} catch (error) {
			failure(error, 'Could not save the decision.');
		}
	}

	// Keeping the tags and leaving it for later can be taken back.
	function offerReopen(review: LibraryReview, action: 'keep_tagged' | 'dismiss'): void {
		const title = review.album_title || 'this album';
		toastStore.show({
			message: action === 'keep_tagged' ? `Tags kept: ${title}.` : `Left for later: ${title}.`,
			type: 'success',
			duration: 8000,
			action: {
				label: 'Reopen',
				run: () =>
					act
						.mutateAsync({
							reviewId: review.id,
							revision: review.row_revision,
							action: 'reopen',
							title: review.album_title
						})
						.catch((error: unknown) => failure(error, "The review couldn't be reopened."))
			}
		});
	}

	function askMessage(review: LibraryReview, action: 'exclude' | 'detach'): string {
		const title = review.album_title || 'This album';
		if (action === 'exclude') {
			return `"${title}" leaves the library. Its files stay on disk, and you can put it back from Excluded.`;
		}
		return review.manual_identity
			? `"${title}" has an edition someone picked by hand. Its pick and MusicBrainz match come off; it keeps its own tags. Undo from the album page.`
			: `"${title}" loses its MusicBrainz match and keeps its own tags. Undo from the album page.`;
	}

	async function choose(review: LibraryReview, candidateKey: string): Promise<void> {
		try {
			await accept.mutateAsync({
				reviewId: review.id,
				revision: review.row_revision,
				candidateKey
			});
			void focusAfter(review.id);
		} catch (error) {
			failure(error, 'Could not use this edition.');
		}
	}

	async function retryOne(review: LibraryReview): Promise<void> {
		try {
			await retry.mutateAsync({
				reviewId: review.id,
				revision: review.row_revision,
				confirmLocalMetadata
			});
		} catch (error) {
			failure(error, 'Could not look again.');
		}
	}

	function toggle(id: string): void {
		allMatching = false;
		selected = selected.includes(id) ? selected.filter((value) => value !== id) : [...selected, id];
	}

	// Bulk changes: the whole selection, or every review of one reason.
	const BULK_LABEL: Record<BulkReviewAction, string> = {
		keep_tagged: 'Keep my tags',
		accept: 'Use the best edition',
		exclude: 'Take out of library'
	};
	let bulkSelection = $state<BulkReviewSelection | null>(null);
	let bulkOpen = $state(false);

	async function previewBulk(action: BulkReviewAction, reasonCode?: string): Promise<boolean> {
		const selection: BulkReviewSelection = reasonCode
			? { action, reviewIds: [], filter: { ...filter, reasonCode } }
			: { action, reviewIds: allMatching ? [] : selected, filter };
		bulkSelection = selection;
		applyBulk.reset();
		try {
			await preview.mutateAsync(selection);
			return true;
		} catch (error) {
			failure(error, 'Could not preview the change.');
			return false;
		}
	}

	async function startBulk(action: BulkReviewAction, reasonCode?: string): Promise<void> {
		if (await previewBulk(action, reasonCode)) bulkOpen = true;
	}

	async function confirmBulk(): Promise<void> {
		if (!bulkSelection || !preview.data) return;
		bulkDone(
			await applyBulk.mutateAsync({
				selection: bulkSelection,
				previewToken: preview.data.preview_token
			})
		);
	}

	// One release for the whole selection, from those every review lists.
	let sharedOpen = $state(false);

	async function startShared(): Promise<void> {
		if (await previewBulk('accept')) sharedOpen = true;
	}

	function bulkDone(result: BulkReviewResult): void {
		const left = result.skipped.reduce((sum, group) => sum + group.count, 0);
		toastStore.show({
			message: `Changing ${plural(result.queued, 'review')} in the background${left ? `; ${left} left alone` : ''}.`,
			type: left ? 'info' : 'success'
		});
		selected = [];
		allMatching = false;
	}

	// Bulk changes and retries run as jobs; each shows here until a few
	// minutes after it ends.
	const reviewJobs = getLibraryReviewJobsQuery(() => authStore.isTrusted);
	const controlJob = createControlLibraryReviewJobMutation();
	const ACTIVE_JOB = ['queued', 'running', 'paused'];
	const JOB_LABEL: Record<NonNullable<LibraryReviewJob['action']>, string> = {
		keep_tagged: 'Keeping the tags',
		accept: 'Using the best edition',
		exclude: 'Taking out of library',
		retry: 'Looking again'
	};
	const JOB_STATE: Record<string, string> = {
		queued: 'Waiting',
		running: 'Running',
		paused: 'Paused',
		succeeded: 'Done',
		stopped: 'Stopped',
		cancelled: 'Stopped',
		failed: 'Failed'
	};
	// Jobs seen working, so the list reloads once when each one ends or pauses.
	const watching: Record<string, boolean> = {};
	$effect(() => {
		for (const job of reviewJobs.data ?? []) {
			if (job.state === 'queued' || job.state === 'running') watching[job.id] = true;
			else if (watching[job.id]) {
				delete watching[job.id];
				void reviewJobEnded(job.action === 'accept' || job.action === 'exclude');
			}
		}
	});

	function controlReviewJob(job: LibraryReviewJob, control: ReviewJobControl): Promise<unknown> {
		return controlJob
			.mutateAsync({ jobId: job.id, control })
			.catch((error: unknown) => failure(error, 'Could not change the job.'));
	}

	// Retrying identification for many: counted first, then started.
	let retryFilter = $state<Omit<LibraryReviewParams, 'limit'>>({});
	let retryOpen = $state(false);

	async function previewRetry(reasonCode?: string): Promise<void> {
		retryFilter = reasonCode ? { ...filter, reasonCode } : filter;
		try {
			await retryAll.mutateAsync({ confirmLocalMetadata, filter: retryFilter, preview: true });
			retryOpen = true;
		} catch (error) {
			failure(error, 'Could not count what would start.');
		}
	}

	async function retryEverything(): Promise<void> {
		const result = await retryAll.mutateAsync({ confirmLocalMetadata, filter: retryFilter });
		toastStore.show({
			message: `Looking again at ${plural(result.started, 'album')} in the background.`,
			type: 'success'
		});
	}

	function reasonCount(code: string): number {
		return firstPage?.reason_counts.find((bucket) => bucket.reason_code === code)?.count ?? 0;
	}
</script>

<div class="space-y-4">
	<p class="max-w-3xl text-sm text-base-content/65">
		Cases that wait on a person: an undone edition with nothing to go back to, editions dropped when
		albums were combined, and repair findings.
	</p>

	<Tabs
		tabs={REVIEW_VIEWS.map((view) => ({
			id: view.state,
			label: view.label,
			count: firstPage ? viewCount(view.state, firstPage.state_counts) : null
		}))}
		selected={filters.state}
		onselect={(state) => (filters.state = state)}
		label="Review views"
		idPrefix="review-views"
	>
		{@render body()}
	</Tabs>
</div>

{#snippet body()}
	<div class="space-y-4 pt-4">
		<div class="flex flex-wrap items-center gap-2">
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
			{#if authStore.isAdmin && pendingView && total > 0}
				<div class="flex flex-wrap items-center gap-3 sm:ml-auto">
					<label class="label cursor-pointer gap-2 text-xs">
						<input
							type="checkbox"
							class="checkbox checkbox-sm"
							bind:checked={confirmLocalMetadata}
						/>
						Include Local metadata folders
					</label>
					<Button size="sm" loading={retryAll.isPending} onclick={() => void previewRetry()}
						>Look again ({total})</Button
					>
				</div>
			{/if}
			<Button
				variant="ghost"
				size="sm"
				icon={SlidersHorizontal}
				class="sm:hidden"
				aria-expanded={filtersOpen}
				aria-controls="review-filters"
				onclick={() => (filtersOpen = !filtersOpen)}>Filters</Button
			>
		</div>

		<div id="review-filters" class={filtersOpen ? '' : 'hidden sm:block'}>
			<ReviewFilterBar {filters} {roots} />
		</div>

		{#if (firstPage?.reason_counts.length ?? 0) > 1 || filters.reasonCode}
			<div class="flex flex-wrap gap-2" role="group" aria-label="Reasons">
				<Button
					size="xs"
					variant={filters.reasonCode ? 'ghost' : 'primary'}
					onclick={() => (filters.reasonCode = '')}>All reasons</Button
				>
				{#each firstPage?.reason_counts ?? [] as bucket (bucket.reason_code)}
					<Button
						size="xs"
						variant={filters.reasonCode === bucket.reason_code ? 'primary' : 'ghost'}
						class="h-auto max-w-full py-1 text-left whitespace-normal"
						title={bucket.reason.action}
						onclick={() => (filters.reasonCode = bucket.reason_code)}
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
				<Button size="sm" disabled={preview.isPending} onclick={() => void startShared()}
					>Use a shared edition</Button
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

		{#if reviewJobs.data?.length}
			<ul class="space-y-2" aria-label="Background review changes">
				{#each reviewJobs.data as job (job.id)}
					{@const active = ACTIVE_JOB.includes(job.state)}
					{@const left = job.skipped.reduce((sum, group) => sum + group.count, 0)}
					<li>
						<Card variant="bordered" padding="sm">
							<div class="grid gap-3 sm:grid-cols-[1fr_auto] sm:items-center">
								<div class="min-w-0 space-y-1.5">
									<div class="flex flex-wrap items-center gap-2">
										<strong class="text-sm"
											>{job.action ? JOB_LABEL[job.action] : 'Changing reviews'}</strong
										>
										<span class="badge badge-ghost badge-sm"
											>{JOB_STATE[job.state] ?? job.state}</span
										>
									</div>
									{#if active}
										<progress
											class="progress progress-primary w-full"
											value={job.completed_count}
											max={Math.max(job.expected_work_count, 1)}
											aria-label="Reviews handled"
										></progress>
									{/if}
									<p class="text-xs text-base-content/60">
										{job.completed_count.toLocaleString()} of {plural(
											job.expected_work_count,
											'review'
										)}{left ? ` · ${left.toLocaleString()} left alone` : ''}
									</p>
									{#if !active && job.skipped.length}
										<ul class="space-y-0.5 text-xs text-base-content/70">
											{#each job.skipped as group (group.reason.code)}
												<li class="flex gap-1.5">
													<CircleAlert class="mt-0.5 h-3.5 w-3.5 shrink-0 text-warning" />
													<span><strong>{group.count}</strong>: {group.reason.message}</span>
												</li>
											{/each}
										</ul>
									{/if}
								</div>
								<OperationJobStrip
									class="sm:justify-end"
									size="xs"
									subject="review change"
									status={job.state}
									controlRequest={job.control_request}
									onpause={() => controlReviewJob(job, 'pause')}
									onresume={() => controlReviewJob(job, 'resume')}
									onstop={() => controlReviewJob(job, 'stop')}
								/>
							</div>
						</Card>
					</li>
				{/each}
			</ul>
		{/if}

		<QueryBoundary {query} what="the reviews" isEmpty={() => reviews.length === 0}>
			{#snippet loading()}
				<div class="space-y-2">
					{#each Array(4) as _, index (index)}<div class="skeleton h-20 w-full"></div>{/each}
				</div>
			{/snippet}
			{#snippet empty()}
				<Card class="text-center text-base-content/60">{EMPTY[filters.state]}</Card>
			{/snippet}
			{#snippet children(loaded)}
				<ul class="space-y-2" bind:this={list}>
					{#each reviews as review, index (review.id)}
						{#if filters.sort === 'reason' && reviews[index - 1]?.reason_code !== review.reason_code}
							<li class="flex flex-wrap items-center gap-2 pt-2">
								<h3 class="mr-auto text-sm font-semibold">
									{review.reason.message}
									<span class="badge badge-sm badge-ghost ml-1"
										>{reasonCount(review.reason_code)}</span
									>
								</h3>
								{#if pendingView && authStore.isTrusted}
									<Button
										size="xs"
										variant="ghost"
										disabled={preview.isPending}
										onclick={() => void startBulk('keep_tagged', review.reason_code)}
										>Keep my tags for all</Button
									>
								{/if}
								{#if pendingView && authStore.isAdmin}
									<Button
										size="xs"
										variant="ghost"
										disabled={retryAll.isPending}
										onclick={() => void previewRetry(review.reason_code)}>Look again at all</Button
									>
								{/if}
							</li>
						{/if}
						<ReviewItem
							{review}
							{busy}
							open={expanded === review.id}
							selectable={canSelect}
							selected={selected.includes(review.id)}
							lockedSelected={allMatching}
							ontoggleopen={() => (expanded = expanded === review.id ? null : review.id)}
							ontoggleselect={() => toggle(review.id)}
							onaction={(action) => void run(review, action)}
							onchoose={(key) => void choose(review, key)}
							onretry={() => void retryOne(review)}
						/>
					{/each}
				</ul>
				{#if total > PAGE}
					<div class="mt-2 flex items-center justify-between text-sm">
						<span class="text-base-content/60"
							>{reviews.length} of {loaded.pages[0]?.total ?? 0}</span
						>
						{#if query.hasNextPage}
							<Button
								size="sm"
								loading={query.isFetchingNextPage}
								onclick={() => void query.fetchNextPage()}>Load more</Button
							>
						{/if}
					</div>
				{/if}
			{/snippet}
		</QueryBoundary>
	</div>
{/snippet}

<ConfirmDialog
	bind:open={askOpen}
	title={asking?.action === 'exclude' ? 'Take out of library?' : 'Remove the match?'}
	message={asking ? askMessage(asking.review, asking.action) : ''}
	confirmLabel={asking?.action === 'exclude' ? 'Take out' : 'Remove match'}
	tone={asking?.action === 'exclude' ? 'danger' : 'primary'}
	onconfirm={() =>
		asking &&
		act.mutateAsync({
			reviewId: asking.review.id,
			revision: asking.review.row_revision,
			action: asking.action,
			title: asking.review.album_title
		})}
/>

<ConfirmDialog
	bind:open={bulkOpen}
	title={bulkSelection ? BULK_LABEL[bulkSelection.action] : ''}
	confirmLabel={`Apply to ${preview.data?.eligible ?? 0}`}
	tone={bulkSelection?.action === 'exclude' ? 'danger' : 'primary'}
	canConfirm={Boolean(preview.data?.eligible)}
	onconfirm={confirmBulk}
>
	{#if preview.data}
		{@const plan = preview.data}
		<p class="text-sm">
			This changes <strong>{plan.eligible}</strong> of {plural(plan.matched, 'review')}.
			{#if bulkSelection?.action === 'accept'}Each album gets its best-scoring edition, chosen so
				automatic matching leaves it alone. Undo each from its album page.{/if}
			{#if bulkSelection?.action === 'exclude'}Their files stay on disk and can be put back from
				Excluded.{/if}
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
	{/if}
</ConfirmDialog>

<ReviewSharedRelease
	bind:open={sharedOpen}
	selection={bulkSelection}
	plan={preview.data ?? null}
	ondone={bulkDone}
/>

<ConfirmDialog
	bind:open={retryOpen}
	title="Look again"
	confirmLabel={`Retry ${retryAll.data?.preview ? retryAll.data.started : 0}`}
	canConfirm={Boolean(retryAll.data?.preview && retryAll.data.started)}
	onconfirm={retryEverything}
>
	{#if retryAll.data?.preview}
		{@const plan = retryAll.data}
		<p class="text-sm">
			<strong>{plural(plan.started, 'album')}</strong> would be looked up on MusicBrainz again, and
			{plan.started === 1 ? 'its review closes' : 'their reviews close'}.
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
</ConfirmDialog>
