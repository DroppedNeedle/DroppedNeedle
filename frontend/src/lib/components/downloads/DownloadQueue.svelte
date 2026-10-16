<script lang="ts">
	import { ChevronDown, Download, RotateCcw, TimerOff, Trash2 } from 'lucide-svelte';
	import { SvelteMap } from 'svelte/reactivity';

	import EmptyState from '$lib/components/EmptyState.svelte';
	import {
		clearFinished,
		retryAllFailed,
		reverifyHeldBulk,
		stopAllRetries
	} from '$lib/queries/downloads/DownloadMutations.svelte';
	import { getDownloadsQuery } from '$lib/queries/downloads/DownloadQueries.svelte';
	import { getHeldImportsQuery } from '$lib/queries/downloads/HeldQueries.svelte';
	import { getQuarantineQuery } from '$lib/queries/downloads/QuarantineQueries.svelte';
	import { bucketSections, collapseRetryChains } from '$lib/queries/downloads/downloadStatus';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { DownloadTask, HeldImport } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';

	import DownloadItem from './DownloadItem.svelte';
	import HeldTrackCard from './HeldTrackCard.svelte';
	import HeldVerdictCard from './HeldVerdictCard.svelte';
	import ManagementHoldCard from './ManagementHoldCard.svelte';
	import NowPressingHero from './NowPressingHero.svelte';
	import QuarantinePanel from './QuarantinePanel.svelte';
	import WantedCard from './WantedCard.svelte';

	const query = getDownloadsQuery();
	const isAdmin = $derived(authStore.isAdmin);
	const quarantineQuery = getQuarantineQuery(() => isAdmin);
	const heldQuery = getHeldImportsQuery();
	const held = $derived(heldQuery.data?.items ?? []);
	const managementHeld = $derived(held.filter((item) => item.reason.startsWith('management:')));
	const verificationHeld = $derived(held.filter((item) => !item.reason.startsWith('management:')));
	const verificationHeldIds = $derived(verificationHeld.map((item) => item.id));
	const managementGroups = $derived.by(() => {
		const groups = new SvelteMap<string, typeof managementHeld>();
		for (const item of managementHeld) {
			const key =
				item.source_task_id ?? `${item.release_group_mbid ?? 'unknown'}:${item.created_at}`;
			groups.set(key, [...(groups.get(key) ?? []), item]);
		}
		return [...groups.values()];
	});
	const heldTaskIds = $derived(
		new Set(held.flatMap((item) => (item.source_task_id ? [item.source_task_id] : [])))
	);
	// Wrong-product verdicts: tasks whose import proved the grabbed folder is a different
	// product collapse to one card with their member tracks (members must still be held -
	// a verdict whose rows were all individually resolved shows nothing).
	const verdictGroups = $derived.by(() => {
		const groups: { task: DownloadTask; items: HeldImport[] }[] = [];
		for (const task of query.data?.items ?? []) {
			if (task.wrong_product_verdict_at == null) continue;
			const items = verificationHeld.filter((item) => item.source_task_id === task.id);
			if (items.length > 0) groups.push({ task, items });
		}
		return groups;
	});
	const verdictMemberIds = $derived(
		new Set(verdictGroups.flatMap((group) => group.items.map((item) => item.id)))
	);
	const loneVerificationHeld = $derived(
		verificationHeld.filter((item) => !verdictMemberIds.has(item.id))
	);

	const clear = clearFinished();
	const stopAll = stopAllRetries();
	const retryAll = retryAllFailed();
	const reverifyBulk = reverifyHeldBulk();

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

	// History = everything terminal. Split the counts so the header is honest about how it went.
	const crateCount = $derived(
		sections.history.filter((t) => t.status === 'completed' || t.status === 'partial').length
	);
	const missedCount = $derived(sections.history.length - crateCount);
	const hasFailed = $derived(sections.history.some((t) => t.status === 'failed'));
	const historyLabel = $derived(
		`${crateCount} in your crate` + (missedCount > 0 ? ` · ${missedCount} didn't make it` : '')
	);

	const pulse = $derived(
		[
			{ n: sections.now_spinning.length, label: 'spinning', cls: 'text-primary' },
			{ n: sections.wanted.length, label: 'still hunting', cls: 'text-warning' },
			{ n: sections.needs_you.length, label: 'needs you', cls: 'text-info' },
			{ n: managementGroups.length, label: 'organizer paused', cls: 'text-warning' },
			{ n: verificationHeld.length, label: 'to verify', cls: 'text-warning' },
			{ n: sections.cueing.length, label: 'cueing up', cls: 'text-base-content/70' },
			{ n: crateCount, label: 'in your crate', cls: 'text-success' }
		].filter((p) => p.n > 0)
	);

	let historyOpen = $state(false);
	let quarantineOpen = $state(false);
</script>

<div class="space-y-7">
	{#if query.isLoading}
		<div class="space-y-3">
			<div class="skeleton h-32 w-full rounded-3xl"></div>
			<div class="skeleton h-20 w-full rounded-2xl"></div>
			<div class="skeleton h-20 w-full rounded-2xl"></div>
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
			<p class="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-base-content/50">
				{#each pulse as p, i (p.label)}
					{#if i > 0}<span class="text-base-content/25">·</span>{/if}
					<span><span class="font-bold tabular-nums {p.cls}">{p.n}</span> {p.label}</span>
				{/each}
			</p>
		{/if}

		<!-- NOW SPINNING -->
		{#if sections.now_spinning.length > 0}
			<section class="space-y-3">
				<h2 class="dl-eyebrow">
					Now spinning <span class="dl-count">{sections.now_spinning.length}</span>
				</h2>
				{#if hero}<NowPressingHero task={hero} showEyebrow={false} />{/if}
				{#each spinningRest as task (task.id)}
					<DownloadItem {task} />
				{/each}
			</section>
		{/if}

		<!-- STILL HUNTING (auto-retry ladder; "Wanted" is the requests page's word) -->
		{#if sections.wanted.length > 0}
			<section class="space-y-3">
				<div class="flex items-center justify-between gap-2">
					<h2 class="dl-eyebrow">
						Still hunting <span class="text-base-content/35">· auto-retrying</span>
						<span class="dl-count">{sections.wanted.length}</span>
					</h2>
					<Button
						variant="ghost"
						size="xs"
						icon={TimerOff}
						onclick={() => stopAll.mutate()}
						disabled={stopAll.isPending}
						title="Stop auto-retrying every still-hunting album - they won't be watched for later either"
					>
						Stop all
					</Button>
				</div>
				<div class="space-y-3">
					{#each sections.wanted as task (task.id)}
						<WantedCard {task} />
					{/each}
				</div>
			</section>
		{/if}

		<!-- NEEDS YOU -->
		{#if sections.needs_you.length > 0}
			<section class="space-y-3">
				<h2 class="dl-eyebrow">
					Needs you <span class="text-base-content/35">· pick a release</span>
					<span class="dl-count">{sections.needs_you.length}</span>
				</h2>
				{#each sections.needs_you as task (task.id)}
					<DownloadItem {task} />
				{/each}
			</section>
		{/if}

		{#if managementGroups.length > 0}
			<section class="space-y-3">
				<h2 class="dl-eyebrow">
					Organizer needs attention <span class="text-base-content/35">· files secured</span>
					<span class="dl-count">{managementGroups.length}</span>
				</h2>
				<div class="space-y-3">
					{#each managementGroups as items (items[0]?.source_task_id ?? items[0]?.id)}
						<ManagementHoldCard {items} />
					{/each}
				</div>
			</section>
		{/if}
		{#if verificationHeld.length > 0}
			<section class="space-y-3">
				<div class="flex items-center justify-between gap-2">
					<h2 class="dl-eyebrow">
						Couldn't verify <span class="text-base-content/35">· your call</span>
						<span class="dl-count">{verificationHeld.length}</span>
					</h2>
					<Button
						variant="ghost"
						size="xs"
						icon={RotateCcw}
						loading={reverifyBulk.isPending}
						onclick={() => reverifyBulk.mutate({ held_ids: verificationHeldIds })}
						title="Run the fingerprint check again on these tracks. Confident matches import automatically"
					>
						{reverifyBulk.isPending ? 'Checking...' : 'Re-check all'}
					</Button>
				</div>
				<div class="space-y-3">
					{#each verdictGroups as group (group.task.id)}
						<HeldVerdictCard task={group.task} items={group.items} />
					{/each}
					{#each loneVerificationHeld as item (item.id)}
						<HeldTrackCard held={item} />
					{/each}
				</div>
			</section>
		{/if}

		<!-- CUEING UP -->
		{#if sections.cueing.length > 0}
			<section class="space-y-3">
				<h2 class="dl-eyebrow">Cueing up <span class="dl-count">{sections.cueing.length}</span></h2>
				{#each sections.cueing as task (task.id)}
					<DownloadItem {task} />
				{/each}
			</section>
		{/if}

		<!-- HISTORY (collapsible) -->
		{#if sections.history.length > 0}
			<section class="space-y-3">
				<div class="flex items-center justify-between gap-2">
					<button
						class="dl-eyebrow flex items-center gap-1.5 hover:text-base-content/70"
						onclick={() => (historyOpen = !historyOpen)}
						aria-expanded={historyOpen}
					>
						<ChevronDown
							class="h-3.5 w-3.5 transition-transform motion-reduce:transition-none {historyOpen
								? ''
								: '-rotate-90'}"
						/>
						History
						<span class="font-normal normal-case tracking-normal text-base-content/40">
							{historyLabel}
						</span>
					</button>
					{#if historyOpen}
						<div class="flex items-center gap-1">
							{#if hasFailed}
								<Button
									variant="ghost"
									size="xs"
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
								size="xs"
								icon={Trash2}
								onclick={() => clear.mutate()}
								disabled={clear.isPending}
								title="Remove completed and cancelled downloads from this list"
							>
								Clear
							</Button>
						</div>
					{/if}
				</div>
				{#if historyOpen}
					<div class="space-y-3">
						{#each sections.history as task (task.id)}
							<DownloadItem {task} />
						{/each}
					</div>
				{/if}
			</section>
		{/if}

		<!-- QUARANTINE (admin, collapsible) -->
		{#if quarantineCount > 0}
			<section class="space-y-3">
				<button
					class="dl-eyebrow flex items-center gap-1.5 hover:text-base-content/70"
					onclick={() => (quarantineOpen = !quarantineOpen)}
					aria-expanded={quarantineOpen}
				>
					<ChevronDown
						class="h-3.5 w-3.5 transition-transform motion-reduce:transition-none {quarantineOpen
							? ''
							: '-rotate-90'}"
					/>
					Quarantine <span class="dl-count">{quarantineCount}</span>
				</button>
				{#if quarantineOpen}<QuarantinePanel />{/if}
			</section>
		{/if}
	{/if}
</div>

<style>
	.dl-eyebrow {
		font-size: 11px;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.16em;
		color: oklch(from var(--color-base-content) l c h / 0.45);
	}
	.dl-count {
		display: inline-block;
		margin-left: 0.35rem;
		padding: 0 0.4rem;
		border-radius: 9999px;
		font-size: 10px;
		font-variant-numeric: tabular-nums;
		letter-spacing: 0;
		color: oklch(from var(--color-base-content) l c h / 0.6);
		background: oklch(from var(--color-base-content) l c h / 0.08);
	}
</style>
