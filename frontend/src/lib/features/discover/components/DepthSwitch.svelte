<!--
	How far Discover's picks go, in the Discover header: home ground, next
	door or far out, with a line saying what the chosen one means. Once a
	change is saved the line also says the next set follows it; this week's
	set never changes.
-->
<script lang="ts">
	import SegmentedChoice from '$lib/ui/SegmentedChoice.svelte';
	import {
		createSaveDiscoverPrefsMutation,
		getDiscoverPrefsQuery,
		type Depth
	} from '$lib/features/discover/queries/week.svelte';

	const prefsQuery = getDiscoverPrefsQuery();
	const save = createSaveDiscoverPrefsMutation();

	const DEPTHS: { value: Depth; label: string; line: string }[] = [
		{ value: 'home_ground', label: 'Home ground', line: 'close to what you play.' },
		{ value: 'next_door', label: 'Next door', line: 'close by.' },
		{ value: 'far_out', label: 'Far out', line: 'further from what you know.' }
	];

	// the depth just chosen shows while it saves, and after
	let chosen = $state<Depth | null>(null);
	let changed = $state(false);
	const depth = $derived(chosen ?? prefsQuery.data?.depth ?? 'next_door');
	const meaning = $derived(DEPTHS.find((each) => each.value === depth) ?? DEPTHS[1]);

	function choose(next: Depth) {
		const prefs = prefsQuery.data;
		if (!prefs || save.isPending || next === depth) return;
		chosen = next;
		save.mutate(
			{ ...prefs, depth: next },
			{
				onSuccess: () => (changed = true),
				onError: () => (chosen = null)
			}
		);
	}
</script>

<div class="dn-depth">
	<div class="dn-depth__row">
		<span class="dn-depth__label" aria-hidden="true">How far</span>
		<SegmentedChoice
			label="How far"
			look="caps"
			options={DEPTHS}
			disabled={!prefsQuery.data || save.isPending}
			bind:value={() => depth, choose}
		/>
	</div>
	<p class="dn-depth__line" aria-live="polite">
		<b>{meaning.label}:</b>
		{meaning.line}
		{#if changed}
			<span>Your next set will follow this. This week holds still.</span>
		{/if}
	</p>
</div>

<style>
	.dn-depth {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.5rem;
		margin-left: auto;
	}
	.dn-depth__row {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}
	.dn-depth__label {
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		letter-spacing: 0.2em;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--dn-muted);
	}
	.dn-depth__line {
		margin: 0;
		font-size: 0.8125rem;
		line-height: 1.45;
		text-align: right;
	}
	.dn-depth__line b {
		color: var(--color-primary);
	}
	.dn-depth__line span {
		display: block;
		color: var(--dn-muted);
	}

	@media (max-width: 639px) {
		.dn-depth {
			order: 5;
			flex-basis: 100%;
			align-items: flex-start;
			gap: 0.375rem;
			margin-left: 0;
		}
		.dn-depth__row {
			flex-wrap: wrap;
			gap: 0.5rem;
		}
		.dn-depth__line {
			text-align: left;
		}
	}
</style>
