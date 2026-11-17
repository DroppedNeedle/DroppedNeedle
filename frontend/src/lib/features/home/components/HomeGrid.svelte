<script lang="ts">
	import 'gridstack/dist/gridstack.min.css';
	import { GridStack } from 'gridstack';
	import type { Snippet } from 'svelte';
	import { REFERENCE_COLUMNS, UNIT, type WidgetPlacement } from '../layout/contract';

	interface Props {
		widgets: WidgetPlacement[];
		children: Snippet<[WidgetPlacement]>;
	}

	let { widgets, children }: Props = $props();

	let host: HTMLDivElement | undefined = $state();
	let grid: GridStack | null = null;

	// square cells: the column count follows the window, the cell is the column width
	function metrics(width: number) {
		const columns = Math.max(6, Math.floor(width / UNIT));
		return { columns, cellHeight: width / columns };
	}

	// gridstack reads these off the element; Svelte types reject them as attributes
	function cellAttrs(widget: WidgetPlacement) {
		return {
			'gs-id': widget.id,
			'gs-x': widget.x,
			'gs-y': widget.y,
			'gs-w': widget.w,
			'gs-h': widget.h,
			'gs-min-w': widget.minW,
			'gs-min-h': widget.minH,
			'gs-max-w': widget.maxW,
			'gs-max-h': widget.maxH
		};
	}

	const structure = $derived(widgets.map((w) => w.id).join('|'));

	$effect(() => {
		const key = structure;
		if (!host || key === '') return;
		const { columns, cellHeight } = metrics(host.clientWidth);
		const instance = GridStack.init(
			{ column: REFERENCE_COLUMNS, cellHeight, margin: 0, mode: 'top', staticGrid: true },
			host
		);
		if (!instance) return;
		grid = instance;
		if (columns !== REFERENCE_COLUMNS) instance.column(columns, 'moveScale');
		return () => {
			instance.destroy(false);
			grid = null;
		};
	});

	$effect(() => {
		if (!host) return;
		const observer = new ResizeObserver(() => {
			if (!host || !grid) return;
			const { columns, cellHeight } = metrics(host.clientWidth);
			grid.column(columns, 'moveScale');
			grid.cellHeight(cellHeight);
		});
		observer.observe(host);
		return () => observer.disconnect();
	});
</script>

<div class="grid-stack home-grid" bind:this={host}>
	{#each widgets as widget (widget.id)}
		<div class="grid-stack-item" {...cellAttrs(widget)}>
			<div class="grid-stack-item-content">
				{@render children(widget)}
			</div>
		</div>
	{/each}
</div>

<style>
	/* the cell clips: a widget that outgrows its rows must not add a scrollbar */
	.home-grid :global(.grid-stack-item-content) {
		overflow: hidden;
	}
</style>
