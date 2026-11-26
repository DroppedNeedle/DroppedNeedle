<!--
	A taste's share of each set as a short bar, its percentage when `words`
	says how to print it, and its trend as a chip: rising in acid, steady
	and fading plain, each with its arrow. Drawn in the paper's ink.
-->
<script lang="ts">
	import { Minus, TrendingDown, TrendingUp } from 'lucide-svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import type { TasteGroup } from '$lib/features/discover/queries/taste.svelte';

	interface Props {
		share: number;
		trend: TasteGroup['trend'];
		/** The percentage's words ("48% of each set"); none prints no number. */
		words?: (percent: number) => string;
		/** A longer bar, for the open panel. */
		wide?: boolean;
	}

	let { share, trend, words, wide = false }: Props = $props();

	const percent = $derived(Math.round(Math.min(Math.max(share, 0), 1) * 100));
	const TRENDS = {
		rising: { word: 'Rising', icon: TrendingUp },
		steady: { word: 'Steady', icon: Minus },
		fading: { word: 'Fading', icon: TrendingDown }
	} as const;
</script>

<span class="dn-taste-share" class:dn-taste-share--wide={wide}>
	<span class="dn-taste-share__bar" aria-hidden="true">
		<span class="dn-taste-share__fill" style:width="{percent}%"></span>
	</span>
	{#if words}<span class="dn-taste-share__words">{words(percent)}</span>{/if}
	<StatusChip size="xs" tone={trend === 'rising' ? 'action' : 'neutral'} icon={TRENDS[trend].icon}>
		{TRENDS[trend].word}
	</StatusChip>
</span>

<style>
	/* on a narrow card the chip goes under the bar rather than past the edge */
	.dn-taste-share {
		display: inline-flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.375rem 0.625rem;
		min-width: 0;
	}
	.dn-taste-share__bar {
		position: relative;
		flex: none;
		width: 5.5rem;
		height: 6px;
		overflow: hidden;
		border-radius: 3px;
		background-color: var(--dn-paper-border);
	}
	.dn-taste-share--wide .dn-taste-share__bar {
		width: 8.75rem;
		height: 8px;
	}
	.dn-taste-share__fill {
		position: absolute;
		inset: 0 auto 0 0;
		background-color: var(--dn-paper-content);
	}
	.dn-taste-share__words {
		white-space: nowrap;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
	@media (forced-colors: active) {
		.dn-taste-share__bar {
			border: 1px solid CanvasText;
		}
		.dn-taste-share__fill {
			background-color: CanvasText;
		}
	}
</style>
