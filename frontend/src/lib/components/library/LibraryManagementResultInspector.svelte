<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { ArrowRight, CircleCheckBig, FileClock, ShieldAlert } from 'lucide-svelte';

	import type { LibraryManagementResultItem } from '$lib/queries/library-management/types';
	import {
		formatManagementValue,
		managementAudioFormat,
		managementPlanAlbum,
		managementPlanArtist,
		managementPlanTitle,
		managementReasonLabel,
		titleManagementValue
	} from './LibraryManagementDisplay';
	import Card from '$lib/ui/Card.svelte';
	import LibraryManagementLyricsEvidence from './LibraryManagementLyricsEvidence.svelte';

	interface Props {
		item: LibraryManagementResultItem;
		operationState?: string | null;
	}

	let { item, operationState = null }: Props = $props();
	const title = $derived(managementPlanTitle(item.plan));
	const artist = $derived(managementPlanArtist(item.plan));
	const album = $derived(managementPlanAlbum(item.plan));
	const status = $derived(item.failure_code ?? item.work_state);
	const failureReason = $derived(
		item.failure_reason?.message ||
			(typeof item.result.reason === 'string' ? item.result.reason : null)
	);
	const failureAction = $derived(item.failure_reason?.action || null);
	const pathChanged = $derived(
		Boolean(item.plan.destination_relative_path) &&
			(item.plan.destination_relative_path !== item.plan.source_relative_path ||
				item.plan.destination_root_id !== item.plan.source_root_id)
	);
</script>

<div class="management-inspector-content">
	<header class="management-inspector-heading">
		<div class="min-w-0 flex-1">
			<p class="management-step">Result evidence</p>
			<h3 class="mt-1 font-display text-lg font-semibold">{title}</h3>
			<p class="mt-1 text-sm text-base-content/55">
				{[artist, album].filter(Boolean).join(' · ') || `Release ${item.plan.bundle_ordinal + 1}`}
			</p>
		</div>
		<div class="flex flex-wrap items-center justify-end gap-1">
			<span class="badge badge-ghost badge-sm font-mono"
				>{managementAudioFormat(item.plan).toUpperCase()}</span
			>
			<span
				class="badge badge-sm {item.failure_code
					? 'badge-error'
					: item.work_state === 'succeeded'
						? 'badge-success'
						: 'badge-outline'}"
			>
				{item.failure_code
					? managementReasonLabel(item.failure_code)
					: titleManagementValue(status)}
			</span>
		</div>
	</header>

	<div class="management-inspector-paths">
		<div>
			<small>Source</small>
			<code>{item.plan.source_relative_path ?? 'No source path'}</code>
		</div>
		{#if pathChanged}
			<ArrowRight class="h-4 w-4 shrink-0 text-library-manage" />
			<div>
				<small>Completed destination</small>
				<code>{item.plan.destination_relative_path}</code>
			</div>
		{/if}
	</div>

	<LibraryManagementLyricsEvidence item={item.plan} workState={item.work_state} {operationState} />

	{#if item.journal_states.length}
		<section>
			<h4 class="management-inspector-section-title">Durable journal</h4>
			<ol class="management-journal-chain">
				{#each item.journal_states as state, index (state)}
					<li>
						<CircleCheckBig class="h-3.5 w-3.5" />
						<span>{titleManagementValue(state)}</span>
						{#if index < item.journal_states.length - 1}<ArrowRight
								class="h-3 w-3 text-base-content/25"
							/>{/if}
					</li>
				{/each}
			</ol>
		</section>
	{/if}

	<section>
		<h4 class="management-inspector-section-title">Recorded outcome</h4>
		{#if item.failure_code}
			<div class="flex items-start gap-2 rounded-xl border border-error/25 bg-error/5 p-3 text-sm">
				<ShieldAlert class="mt-0.5 h-4 w-4 shrink-0 text-error" />
				<div>
					<strong>{managementReasonLabel(item.failure_code)}</strong>
					<p class="mt-1 text-base-content/70">
						{failureReason ?? 'This file was not written.'}
					</p>
					{#if failureAction}
						<p class="mt-1 text-base-content/55">
							<span class="font-semibold">What to do:</span>
							{failureAction}
						</p>
					{/if}
				</div>
			</div>
		{:else if item.result && Object.keys(item.result).length}
			<Card variant="bordered" padding="sm">
				<div class="flex items-center gap-2">
					<FileClock class="h-4 w-4 text-library-manage" /><strong class="text-sm"
						>Result payload</strong
					>
				</div>
				<p class="mt-2 break-words font-mono text-xs text-base-content/60">
					{formatManagementValue(item.result)}
				</p>
			</Card>
		{:else}
			<p class="text-sm text-base-content/50">No additional result payload was required.</p>
		{/if}
	</section>
</div>

<style>
	.management-journal-chain {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.35rem;
		font-size: 0.72rem;
	}

	.management-journal-chain li {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		color: color-mix(in oklab, var(--color-success) 70%, var(--color-base-content));
	}
</style>
