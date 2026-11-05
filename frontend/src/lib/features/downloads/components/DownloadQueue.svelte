<script lang="ts">
	import { Download, RotateCcw, Square, Trash2 } from 'lucide-svelte';

	import EmptyState from '$lib/ui/EmptyState.svelte';
	import {
		createClearFinishedMutation,
		createRetryAllFailedMutation,
		createStopAllRetriesMutation
	} from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import { getDownloadsQuery } from '$lib/features/downloads/queries/DownloadQueries.svelte';
	import {
		getHeldEntriesQuery,
		createRecheckAllHeldMutation
	} from '$lib/features/downloads/queries/HeldQueries.svelte';
	import { getQuarantineQuery } from '$lib/features/downloads/queries/QuarantineQueries.svelte';
	import {
		bucketSections,
		collapseRetryChains,
		isSettled,
		liveTwin
	} from '$lib/features/downloads/queries/downloadStatus';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';

	import DownloadItem from './DownloadItem.svelte';
	import HeldEntryCard from './HeldEntryCard.svelte';
	import NowPressingHero from './NowPressingHero.svelte';
	import QuarantinePanel from './QuarantinePanel.svelte';
	import QueueSectionHead from './QueueSectionHead.svelte';
	import WantedCard from './WantedCard.svelte';

	const query = getDownloadsQuery();
	const isAdmin = $derived(authStore.isAdmin);
	const quarantineQuery = getQuarantineQuery(() => isAdmin);
	// Every download and upload whose files wait for a decision, one entry each.
	const heldQuery = getHeldEntriesQuery();
	const held = $derived(heldQuery.data?.entries ?? []);
	const heldTaskIds = $derived(
		new Set(
			held.flatMap((entry) =>
				entry.files.flatMap((file) => (file.source_task_id ? [file.source_task_id] : []))
			)
		)
	);
	// A download that grabbed the wrong edition can hunt again from its entry.
	const verdictTaskIds = $derived(
		new Set(
			(query.data?.items ?? [])
				.filter((task) => task.wrong_product_verdict_at != null)
				.map((task) => task.id)
		)
	);
	function verdictTaskOf(entry: (typeof held)[number]): string | null {
		if (entry.origin !== 'download') return null;
		return (
			entry.files.find((file) => file.source_task_id && verdictTaskIds.has(file.source_task_id))
				?.source_task_id ?? null
		);
	}
	const recheckEntryIds = $derived(
		held
			.filter((entry) => entry.files.some((file) => file.actions.includes('recheck')))
			.map((entry) => entry.id)
	);

	const clear = createClearFinishedMutation();
	const stopAll = createStopAllRetriesMutation();
	const retryAll = createRetryAllFailedMutation();
	const recheckAll = createRecheckAllHeldMutation();

	// collapse auto-retry chains so each album is one row (latest attempt), then group into
	// the dashboard's stacked sections
	const tasks = $derived(
		collapseRetryChains(query.data?.items ?? []).filter((task) => !heldTaskIds.has(task.id))
	);
	const sections = $derived(bucketSections(tasks));

	const hero = $derived(sections.now_spinning[0] ?? null);
	const spinningRest = $derived(sections.now_spinning.slice(1));
	const quarantineCount = $derived(isAdmin ? (quarantineQuery.data?.items.length ?? 0) : 0);
	// "nothing at all" - no downloads, nothing held for review, and (for admins) nothing quarantined
	const isEmpty = $derived(tasks.length === 0 && held.length === 0 && quarantineCount === 0);

	// History = everything terminal. Split the counts so the header is honest about how it went:
	// a download the app stopped because nothing was left to do is neither.
	const crateCount = $derived(
		sections.history.filter((t) => t.status === 'completed' || t.status === 'partial').length
	);
	const missedCount = $derived(
		sections.history.filter(
			(t) => t.status === 'failed' || (t.status === 'cancelled' && !isSettled(t))
		).length
	);
	const hasFailed = $derived(sections.history.some((t) => t.status === 'failed'));
	const historyLabel = $derived(
		[
			crateCount > 0 ? `${crateCount} in your crate` : null,
			missedCount > 0 ? `${missedCount} didn't make it` : null
		]
			.filter(Boolean)
			.join(' · ')
	);

	const pulse = $derived(
		[
			{ n: sections.now_spinning.length, label: 'spinning', tone: 'live' },
			{ n: sections.wanted.length, label: 'still hunting', tone: 'wait' },
			{ n: sections.needs_you.length, label: 'needs you', tone: 'wait' },
			{ n: held.length, label: 'to decide', tone: 'wait' },
			{ n: sections.cueing.length, label: 'cueing up', tone: 'quiet' },
			{ n: crateCount, label: 'in your crate', tone: 'done' }
		].filter((p) => p.n > 0)
	);

	let historyOpen = $state(false);
	let quarantineOpen = $state(false);
</script>

<div class="space-y-9">
	{#if query.isLoading}
		<div class="space-y-4" aria-busy="true">
			<div class="skeleton h-5 w-80 max-w-full rounded"></div>
			<div class="skeleton h-44 w-full rounded-box"></div>
			<div class="skeleton h-24 w-full rounded-box"></div>
			<div class="skeleton h-24 w-full rounded-box"></div>
		</div>
	{:else if query.isError}
		<div class="alert alert-error">Couldn't load your downloads - retrying shortly.</div>
	{:else if isEmpty}
		<EmptyState
			icon={Download}
			title="Nothing on the turntable"
			description="Request an album and you'll watch it search, download, and land in your library here."
			ctaLabel="Browse Library"
			ctaHref="/library/albums"
		/>
	{:else}
		{#if pulse.length > 0}
			<p class="dl-pulse">
				{#each pulse as p, i (p.label)}
					{#if i > 0}<span class="dl-pulse__dot" aria-hidden="true">·</span>{/if}
					<span class="dl-pulse__part" data-tone={p.tone}
						><span class="dl-pulse__n">{p.n}</span> {p.label}</span
					>
				{/each}
			</p>
		{/if}

		<!-- NOW SPINNING -->
		{#if sections.now_spinning.length > 0}
			<section>
				<QueueSectionHead title="Now spinning" count={sections.now_spinning.length} />
				<div class="space-y-3">
					{#if hero}<NowPressingHero task={hero} />{/if}
					{#each spinningRest as task (task.id)}
						<DownloadItem {task} />
					{/each}
				</div>
			</section>
		{/if}

		<!-- STILL HUNTING (auto-retry ladder; "Wanted" is the requests page's word) -->
		{#if sections.wanted.length > 0}
			<section>
				<QueueSectionHead
					title="Still hunting"
					aside="auto-retrying"
					count={sections.wanted.length}
				>
					{#snippet actions()}
						<Button
							variant="outline"
							size="sm"
							icon={Square}
							onclick={() => stopAll.mutate()}
							disabled={stopAll.isPending}
							title="Stop auto-retrying every still-hunting album - they won't be watched for later either"
						>
							Stop all
						</Button>
					{/snippet}
				</QueueSectionHead>
				<div class="space-y-3">
					{#each sections.wanted as task (task.id)}
						<WantedCard {task} />
					{/each}
				</div>
			</section>
		{/if}

		<!-- NEEDS YOU -->
		{#if sections.needs_you.length > 0}
			<section>
				<QueueSectionHead
					title="Needs you"
					aside="pick a release"
					count={sections.needs_you.length}
				/>
				<div class="space-y-3">
					{#each sections.needs_you as task (task.id)}
						<DownloadItem {task} />
					{/each}
				</div>
			</section>
		{/if}

		<!-- HELD: files waiting for a decision -->
		{#if held.length > 0}
			<section>
				<QueueSectionHead title="To decide" aside="files kept" count={held.length}>
					{#snippet actions()}
						{#if recheckEntryIds.length > 0}
							<Button
								variant="outline"
								size="sm"
								icon={RotateCcw}
								loading={recheckAll.isPending}
								onclick={() => recheckAll.mutate(recheckEntryIds)}
								title="Ask AcoustID again. Files it now confirms are imported"
							>
								{recheckAll.isPending ? 'Checking...' : 'Re-check all'}
							</Button>
						{/if}
					{/snippet}
				</QueueSectionHead>
				<div class="space-y-3">
					{#each held as entry (entry.id)}
						<HeldEntryCard {entry} retryTaskId={verdictTaskOf(entry)} />
					{/each}
				</div>
			</section>
		{/if}

		<!-- CUEING UP -->
		{#if sections.cueing.length > 0}
			<section>
				<QueueSectionHead title="Cueing up" count={sections.cueing.length} />
				<div class="space-y-3">
					{#each sections.cueing as task (task.id)}
						<DownloadItem {task} />
					{/each}
				</div>
			</section>
		{/if}

		<!-- HISTORY and QUARANTINE: collapsed until opened -->
		{#if sections.history.length > 0 || quarantineCount > 0}
			<div class={historyOpen || quarantineOpen ? 'space-y-9' : 'space-y-4'}>
				{#if sections.history.length > 0}
					<section>
						<QueueSectionHead
							title="History"
							note={historyLabel}
							open={historyOpen}
							ontoggle={() => (historyOpen = !historyOpen)}
						>
							{#snippet actions()}
								{#if historyOpen}
									{#if hasFailed}
										<Button
											variant="outline"
											size="sm"
											icon={RotateCcw}
											onclick={() => retryAll.mutate()}
											disabled={retryAll.isPending}
											title="Retry every failed download"
										>
											Retry all failed
										</Button>
									{/if}
									<Button
										variant="ghost"
										size="sm"
										icon={Trash2}
										onclick={() => clear.mutate()}
										disabled={clear.isPending}
										title="Remove completed and cancelled downloads from this list"
									>
										Clear
									</Button>
								{/if}
							{/snippet}
						</QueueSectionHead>
						{#if historyOpen}
							<div class="space-y-3">
								{#each sections.history as task (task.id)}
									<DownloadItem {task} liveTaskId={liveTwin(task, tasks)?.id ?? null} />
								{/each}
							</div>
						{/if}
					</section>
				{/if}

				<!-- admins only -->
				{#if quarantineCount > 0}
					<section>
						<QueueSectionHead
							title="Quarantine"
							count={quarantineCount}
							open={quarantineOpen}
							ontoggle={() => (quarantineOpen = !quarantineOpen)}
						/>
						{#if quarantineOpen}<QuarantinePanel />{/if}
					</section>
				{/if}
			</div>
		{/if}
	{/if}
</div>

<style>
	.dl-pulse {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0.25rem 1rem;
		font-family: var(--font-mono);
		font-size: 1.125rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}
	.dl-pulse__dot {
		color: var(--dn-muted);
	}
	.dl-pulse__part {
		white-space: nowrap;
	}
	.dl-pulse__part[data-tone='live'],
	.dl-pulse__n {
		color: var(--dn-title);
	}
	.dl-pulse__part[data-tone='wait'],
	.dl-pulse__part[data-tone='wait'] .dl-pulse__n {
		color: var(--color-warning);
	}
	.dl-pulse__part[data-tone='quiet'] .dl-pulse__n {
		color: var(--color-base-content);
	}
	@media (max-width: 639px) {
		.dl-pulse {
			gap: 0.125rem 0.625rem;
			font-size: 0.8125rem;
			letter-spacing: 0.08em;
		}
	}
</style>
