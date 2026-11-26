<!--
	What the person changed on their taste, newest first: one line an
	artist, a whole group's statement on one line under its name, each with
	the day and Undo, the newest few until asked for all, and the way to
	everything they told Discover. Nothing while they changed nothing.
-->
<script lang="ts">
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { createUndoTuningMutation } from '$lib/features/discover/queries/DiscoverV3Mutations.svelte';
	import { getTuningsQuery } from '$lib/features/discover/queries/DiscoverV3Queries.svelte';
	import { TUNING_WORDS, tuningLines, undoLabel } from '$lib/features/discover/state/tuningWords';
	import { dayMonth } from '$lib/features/discover/state/weekWords';
	import { withBasePath } from '$lib/utils/basePath';

	/** Lines shown until asked for all. */
	const SHOWN = 6;

	const tuningsQuery = getTuningsQuery(() => true);
	const undo = createUndoTuningMutation();
	let all = $state(false);

	const lines = $derived(tuningLines(tuningsQuery.data?.items ?? []));
</script>

{#if lines.length}
	<Card variant="bordered" padding="md" as="section" aria-labelledby="taste-changes-head">
		<h2 id="taste-changes-head" class="dn-taste-changes__head">You changed</h2>
		<ul class="dn-taste-changes">
			{#each all ? lines : lines.slice(0, SHOWN) as line (line.key)}
				<li>
					<span class="dn-taste-changes__what">
						<b>{line.name}</b>
						<span>{TUNING_WORDS[line.kind].word}</span>
					</span>
					<span class="dn-taste-changes__when">{dayMonth(line.created_at * 1000)}</span>
					<AsyncButton
						variant="caps-link"
						size="xs"
						aria-label={undoLabel(line.kind, line.name)}
						onclick={() =>
							undo
								.mutateAsync({ kind: line.kind, target: line.target, group: line.group })
								.catch(() => undefined)}
					>
						Undo
					</AsyncButton>
				</li>
			{/each}
		</ul>
		{#if lines.length > SHOWN && !all}
			<Button variant="caps" size="xs" onclick={() => (all = true)}>Show all</Button>
		{/if}
		<Button variant="caps-link" size="xs" href={withBasePath('/discover/told')}>
			All in What you told Discover
		</Button>
	</Card>
{/if}

<style>
	.dn-taste-changes__head {
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-taste-changes {
		margin: 0.5rem 0;
		padding: 0;
		list-style: none;
	}
	.dn-taste-changes li {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.5rem 0;
		border-top: 1px solid var(--dn-line);
		font-size: 0.875rem;
	}
	.dn-taste-changes__what {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.dn-taste-changes__what span,
	.dn-taste-changes__when {
		color: var(--dn-muted);
	}
	.dn-taste-changes__when {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		text-transform: uppercase;
	}
</style>
