<!--
	A row of counts: each a big Anton number over a mono caps label, split by
	thin rules. Real counts only; a caller leaves out a count it does not
	have rather than show a zero it guessed. On a narrow screen the row keeps
	its columns and the numbers step down; with `phoneRows` a row of more
	than three counts goes into rows of three on a phone instead, so every
	label reads whole.
-->
<script lang="ts" module>
	export interface Stat {
		label: string;
		value: number | string;
	}
</script>

<script lang="ts">
	interface Props {
		stats: Stat[];
		/** What the counts are of, for screen readers. */
		label: string;
		/** On a phone, rows of three counts rather than one row. */
		phoneRows?: boolean;
		/** Layout only: margin, alignment. */
		class?: string;
	}

	let { stats, label, phoneRows = false, class: className = '' }: Props = $props();
</script>

{#if stats.length > 0}
	<dl class="dn-stats {className}" data-phone-rows={phoneRows ? '' : undefined} aria-label={label}>
		{#each stats as stat (stat.label)}
			<div class="dn-stats__item">
				<dt class="dn-stats__label">{stat.label}</dt>
				<dd class="dn-stats__value">{stat.value}</dd>
			</div>
		{/each}
	</dl>
{/if}

<style>
	.dn-stats {
		display: grid;
		grid-auto-flow: column;
		grid-auto-columns: minmax(0, 1fr);
	}

	.dn-stats__item {
		display: flex;
		flex-direction: column-reverse;
		justify-content: flex-end;
		gap: 0.375rem;
		min-width: 0;
		padding-inline: 0.875rem;
		border-left: 1px solid var(--dn-line);
	}
	.dn-stats__item:first-child {
		border-left: 0;
		padding-left: 0;
	}

	.dn-stats__value {
		font-family: var(--font-display);
		font-size: 1.75rem;
		line-height: 1;
		letter-spacing: 0.01em;
		color: var(--color-base-content);
		font-variant-numeric: tabular-nums;
		font-synthesis: none;
	}

	.dn-stats__label {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--color-base-content);
	}

	@media (max-width: 639px) {
		.dn-stats[data-phone-rows] {
			grid-auto-flow: row;
			grid-template-columns: repeat(3, minmax(0, 1fr));
			row-gap: 1rem;
		}
		.dn-stats[data-phone-rows] .dn-stats__item:nth-child(3n + 1) {
			border-left: 0;
			padding-left: 0;
		}
	}

	@media (min-width: 1024px) {
		.dn-stats {
			grid-auto-columns: auto;
		}
		.dn-stats__item {
			padding-inline: 1.5rem;
		}
		.dn-stats__value {
			font-size: 2rem;
		}
	}
</style>
