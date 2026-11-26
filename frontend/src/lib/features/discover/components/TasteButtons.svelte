<!--
	More, Less and That was a phase for one artist or a whole taste group,
	as paper toggles: the statement in effect is pressed, and pressing it
	again takes it back. On a phone "That was a phase" reads "A phase".
-->
<script lang="ts">
	import { Minus, Plus, Hourglass } from 'lucide-svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';
	import Button from '$lib/ui/Button.svelte';
	import {
		ARTIST_TUNINGS,
		TUNING_WORDS,
		type ArtistTuning
	} from '$lib/features/discover/state/tuningWords';

	interface Props {
		/** What the buttons are about, for the group's name. */
		label: string;
		/** The statement in effect; none while there is none. */
		current: ArtistTuning | null;
		/** Blocks the buttons while a change is on its way. */
		busy?: boolean;
		onchoose: (kind: ArtistTuning) => void;
		onundo: (kind: ArtistTuning) => void;
	}

	let { label, current, busy = false, onchoose, onundo }: Props = $props();

	const ICONS: Record<ArtistTuning, KitIcon> = {
		more_from_seed: Plus,
		less_from_seed: Minus,
		phase: Hourglass
	};
</script>

<div class="dn-taste-buttons" role="group" aria-label={label}>
	{#each ARTIST_TUNINGS as kind (kind)}
		{@const on = current === kind}
		<Button
			variant="paper"
			size="sm"
			icon={ICONS[kind]}
			active={on}
			aria-pressed={on}
			aria-label={TUNING_WORDS[kind].button}
			disabled={busy}
			onclick={() => (on ? onundo(kind) : onchoose(kind))}
		>
			<span class="dn-taste-buttons__full">{TUNING_WORDS[kind].button}</span>
			<span class="dn-taste-buttons__short" aria-hidden="true">{TUNING_WORDS[kind].short}</span>
		</Button>
	{/each}
</div>

<style>
	.dn-taste-buttons {
		display: flex;
		flex-wrap: wrap;
		gap: 0.375rem;
	}
	.dn-taste-buttons__short {
		display: none;
	}
	@media (max-width: 639px) {
		.dn-taste-buttons__full {
			display: none;
		}
		.dn-taste-buttons__short {
			display: inline;
		}
	}
</style>
