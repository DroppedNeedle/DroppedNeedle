<script lang="ts">
	import { Check, Circle } from 'lucide-svelte';
	import type { UpgradeRun, UpgradeStep } from '$lib/queries/upgrade/UpgradeQueries.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { STEP_LABELS, plannedSteps } from './upgradeText';

	interface Props {
		run: UpgradeRun;
		/** The last progress read failed: the page lost its connection. */
		disconnected: boolean;
	}

	let { run, disconnected }: Props = $props();

	const TITLES = { upgrade: 'Upgrading', fresh: 'Starting fresh', undo: 'Going back to v2' };

	// Steps a run passed through, for the ones that only some runs take.
	let seen = $state<UpgradeStep[]>([]);
	$effect(() => {
		const step = run.step;
		if (step && !seen.includes(step)) seen = [...seen, step];
	});

	const rollingBack = $derived(run.step === 'roll_back');
	const steps = $derived(
		rollingBack ? (['roll_back'] as UpgradeStep[]) : plannedSteps(run.task, run.step, seen)
	);
	const current = $derived(run.step ? steps.indexOf(run.step) : -1);
</script>

<Card variant="bordered" title={TITLES[run.task]}>
	{#if rollingBack}
		<p class="text-sm text-base-content/70">Something went wrong. Putting your v2 files back.</p>
	{:else}
		<p class="text-sm text-base-content/70">This carries on if you close the page.</p>
	{/if}
	<ol class="mt-3 flex flex-col gap-3" aria-label="Steps">
		{#each steps as step, index (step)}
			{@const state = index < current ? 'done' : index === current ? 'current' : 'waiting'}
			<li class="flex items-start gap-3" aria-current={state === 'current' ? 'step' : undefined}>
				{#if state === 'done'}
					<Check class="mt-0.5 size-4 shrink-0 text-success" aria-label="Done" />
				{:else if state === 'current'}
					<span class="loading loading-spinner loading-xs motion-keep mt-0.5 shrink-0 text-primary"
					></span>
				{:else}
					<Circle class="mt-0.5 size-4 shrink-0 text-base-content/30" aria-label="Waiting" />
				{/if}
				<div class="min-w-0 flex-1">
					<p class={state === 'waiting' ? 'text-base-content/50' : ''}>{STEP_LABELS[step]}</p>
					{#if state === 'current'}
						{#if run.total}
							<div class="mt-1 flex items-center gap-2">
								<progress
									class="progress progress-primary w-full"
									value={run.done ?? 0}
									max={run.total}
								></progress>
								<span class="shrink-0 whitespace-nowrap font-mono text-xs text-base-content/60">
									{run.done ?? 0} / {run.total}
								</span>
							</div>
						{:else}
							<progress class="progress progress-primary motion-keep mt-1 w-full"></progress>
						{/if}
					{/if}
				</div>
			</li>
		{/each}
	</ol>
	{#if disconnected}
		<p class="mt-3 text-sm text-warning" role="status">Lost the connection. Still trying.</p>
	{/if}
</Card>
