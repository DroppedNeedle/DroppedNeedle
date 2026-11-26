<!--
	How long a record has sat quiet: one tick a month from `from` (the first
	month of the person's history) to `now`, at most `most` of them, the
	last ones kept. The month of the last play is solid, the months since
	are hatched, and the first and last months are named under the ticks. It
	is one image named by `label` ("Last played in February, quiet for nine
	months"), drawn in the text colour. Months are `YYYY-MM`.
-->
<script lang="ts">
	interface Props {
		/** The first month to draw. */
		from: string;
		/** The month it was last played. */
		last: string;
		/** This month, the last tick. */
		now: string;
		/** The sentence the drawing says. */
		label: string;
		/** The most ticks drawn. */
		most?: number;
		/** Layout only: width, margin. */
		class?: string;
	}

	let { from, last, now, label, most = 12, class: className = '' }: Props = $props();

	const MONTHS = [
		'Jan',
		'Feb',
		'Mar',
		'Apr',
		'May',
		'Jun',
		'Jul',
		'Aug',
		'Sep',
		'Oct',
		'Nov',
		'Dec'
	] as const;

	/** Months since year 0 of a `YYYY-MM` month; null when it does not read. */
	function index(month: string): number | null {
		const match = /^(\d{4})-(\d{2})$/.exec(month);
		if (!match) return null;
		const at = Number(match[1]) * 12 + Number(match[2]) - 1;
		return Number.isFinite(at) ? at : null;
	}

	const end = $derived(index(now) ?? 0);
	const start = $derived(
		Math.max(end - Math.max(1, Math.round(most)) + 1, Math.min(index(from) ?? end, end))
	);
	const played = $derived(index(last));
	const ticks = $derived(
		Array.from({ length: end - start + 1 }, (_, i) => {
			const at = start + i;
			const look = played === null ? 'hush' : at === played ? 'on' : at > played ? 'hush' : '';
			return { at, look };
		})
	);
	const name = (at: number) => MONTHS[((at % 12) + 12) % 12];
</script>

<div class="dn-quiet {className}" role="img" aria-label={label} style:--quiet-count={ticks.length}>
	<span class="dn-quiet__ticks" aria-hidden="true">
		{#each ticks as tick (tick.at)}
			<i data-look={tick.look || undefined}></i>
		{/each}
	</span>
	<span class="dn-quiet__ends" aria-hidden="true">
		<span>{name(start)}</span>
		{#if end > start}<span>{name(end)}</span>{/if}
	</span>
</div>

<style>
	.dn-quiet {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.dn-quiet__ticks {
		display: grid;
		grid-template-columns: repeat(var(--quiet-count), minmax(0, 1fr));
		gap: 3px;
	}
	.dn-quiet__ticks i {
		height: 0.625rem;
		border-radius: 1px;
		background: color-mix(in oklab, currentColor 14%, transparent);
	}
	.dn-quiet__ticks i[data-look='on'] {
		background: currentColor;
	}
	.dn-quiet__ticks i[data-look='hush'] {
		background: repeating-linear-gradient(
			90deg,
			color-mix(in oklab, currentColor 30%, transparent) 0 2px,
			transparent 2px 5px
		);
	}

	.dn-quiet__ends {
		display: flex;
		justify-content: space-between;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 500;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	@media (forced-colors: active) {
		.dn-quiet__ticks i {
			background: none;
			border: 1px solid CanvasText;
		}
		.dn-quiet__ticks i[data-look='on'] {
			background: CanvasText;
		}
	}
</style>
