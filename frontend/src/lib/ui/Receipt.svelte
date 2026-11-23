<!--
	A full till receipt: a title and a line under it, then sections of rows,
	each a label and its values in mono with the values set right. A section
	head may carry its own value ("PICKED 8") or name the value columns
	("SHARE", "PICKS"). It is a table, so a screen reader reads each value
	with its row and column. It unrolls like a printer, in steps over 0.7s,
	when it first shows; under reduced motion it is simply there. The barcode
	at its foot is decoration.
-->
<script lang="ts" module>
	export interface ReceiptRow {
		label: string;
		values: string[];
	}

	export interface ReceiptSection {
		head: string;
		/** The head's own value, printed as a sum. */
		value?: string;
		/** Names of the value columns, printed on the head's line. */
		columns?: string[];
		rows?: ReceiptRow[];
	}
</script>

<script lang="ts">
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		title: string;
		subtitle?: string;
		sections: ReceiptSection[];
		/** Layout only: width, margin. */
		class?: string;
	}

	let { title, subtitle, sections, class: className = '' }: Props = $props();

	// every section's values line up in the same columns
	const width = $derived(
		Math.max(
			1,
			...sections.flatMap((s) => [
				s.columns?.length ?? 0,
				...(s.rows ?? []).map((r) => r.values.length)
			])
		)
	);
	const columnsOf = (named: string[]) => Array.from({ length: width }, (_, c) => named[c] ?? '');
	const bars = `url("${withBasePath('/img/barcode.svg')}")`;
</script>

<div class="dn-till {className}">
	<div class="dn-till__paper">
		<p class="dn-till__title">{title}</p>
		{#if subtitle}<p class="dn-till__sub">{subtitle}</p>{/if}
		<table class="dn-till__table" aria-label={subtitle ? `${title}, ${subtitle}` : title}>
			{#each sections as section, i (i)}
				<tbody>
					<tr class="dn-till__head" class:is-sum={section.value !== undefined}>
						{#if section.value !== undefined}
							<th scope="row">{section.head}</th>
							<td colspan={width}>{section.value}</td>
						{:else if section.columns}
							<th scope="col">{section.head}</th>
							{#each columnsOf(section.columns) as column, c (c)}
								<th scope="col" class="dn-till__col">{column}</th>
							{/each}
						{:else}
							<th scope="rowgroup" colspan={width + 1}>{section.head}</th>
						{/if}
					</tr>
					{#each section.rows ?? [] as row, r (r)}
						<tr>
							<th scope="row">{row.label}</th>
							{#each row.values as value, c (c)}
								<td colspan={c === row.values.length - 1 ? width - c : 1}>{value}</td>
							{/each}
						</tr>
					{/each}
				</tbody>
			{/each}
		</table>
		<span
			class="dn-till__bars dn-decor"
			aria-hidden="true"
			style:mask-image={bars}
			style:-webkit-mask-image={bars}
		></span>
	</div>
</div>

<style>
	.dn-till {
		filter: drop-shadow(0 30px 40px rgb(0 0 0 / 0.45));
		animation: dn-till-unroll 700ms steps(14) both;
	}
	@keyframes dn-till-unroll {
		from {
			clip-path: inset(-3rem -3rem 100% -3rem);
		}
		to {
			clip-path: inset(-3rem);
		}
	}

	.dn-till__paper {
		padding: 1.625rem 1.625rem 2.125rem;
		background-color: var(--dn-paper-white);
		color: var(--dn-paper-content);
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 500;
		line-height: 1.7;
		clip-path: polygon(
			0 0,
			100% 0,
			100% 99%,
			96% 100%,
			92% 99%,
			88% 100%,
			84% 99%,
			80% 100%,
			76% 99%,
			72% 100%,
			68% 99%,
			64% 100%,
			60% 99%,
			56% 100%,
			52% 99%,
			48% 100%,
			44% 99%,
			40% 100%,
			36% 99%,
			32% 100%,
			28% 99%,
			24% 100%,
			20% 99%,
			16% 100%,
			12% 99%,
			8% 100%,
			4% 99%,
			0 100%
		);
	}

	.dn-till__title {
		margin: 0;
		font-family: var(--font-display);
		font-size: 2.125rem;
		font-weight: 400;
		line-height: 1.1;
		text-align: center;
		text-transform: uppercase;
		font-synthesis: none;
	}
	.dn-till__sub {
		margin: 0;
		font-size: 0.71875rem;
		letter-spacing: 0.1em;
		text-align: center;
		text-transform: uppercase;
	}

	.dn-till__table {
		width: 100%;
		margin-top: 0.625rem;
		border-collapse: collapse;
	}
	.dn-till__table th,
	.dn-till__table td {
		padding: 0;
		font-weight: 500;
		text-align: left;
		vertical-align: baseline;
	}
	.dn-till__table td,
	.dn-till__table .dn-till__col {
		padding-left: 0.875rem;
		text-align: right;
		font-weight: 700;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.dn-till__table tr:not(.dn-till__head) th {
		padding-left: 0.5rem;
	}
	.dn-till__head > * {
		padding-top: 0.625rem;
		border-top: 1.5px dashed var(--dn-paper-content);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	.dn-till__table .dn-till__col {
		font-size: 0.625rem;
		font-weight: 600;
		letter-spacing: 0.12em;
		color: color-mix(in oklab, var(--dn-paper-content) 74%, var(--dn-paper-white));
	}
	.dn-till__head.is-sum > * {
		font-size: 1rem;
		letter-spacing: 0.04em;
	}

	.dn-till__bars {
		display: block;
		width: 78%;
		height: 2.375rem;
		margin: 0.875rem auto 0;
		background-color: currentColor;
		mask-size: 100% 100%;
		mask-repeat: no-repeat;
		-webkit-mask-size: 100% 100%;
		-webkit-mask-repeat: no-repeat;
	}

	@media (prefers-contrast: more) {
		.dn-till {
			filter: none;
		}
		.dn-till__paper {
			clip-path: none;
			border: 1px solid currentColor;
		}
	}
</style>
