<script lang="ts">
	import 'gridstack/dist/gridstack.min.css';
	import { GridStack, type GridItemHTMLElement, type GridStackNode } from 'gridstack';
	import type { Snippet } from 'svelte';
	import { REFERENCE_COLUMNS, UNIT, type WidgetPlacement } from '../layout/contract';

	interface Props {
		widgets: WidgetPlacement[];
		editing?: boolean;
		/** Placements in reference columns, after a drag or a resize. */
		onchange?: (moved: { id: string; x: number; y: number; w: number; h: number }[]) => void;
		children: Snippet<[WidgetPlacement]>;
	}

	let { widgets, editing = false, onchange, children }: Props = $props();

	let host: HTMLDivElement | undefined = $state();
	let grid = $state.raw<GridStack | null>(null);

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

	// the element under the pointer at release, so the drop can glide from it
	let dragFrom: DOMRect | null = null;

	function flip(el: HTMLElement) {
		const from = dragFrom;
		dragFrom = null;
		if (!from) return;
		const to = el.getBoundingClientRect();
		const dx = from.left - to.left;
		const dy = from.top - to.top;
		if (Math.abs(dx) < 1 && Math.abs(dy) < 1) return;
		el.animate([{ transform: `translate(${dx}px, ${dy}px)` }, { transform: 'translate(0, 0)' }], {
			duration: 220,
			easing: 'cubic-bezier(0.2, 0.7, 0.3, 1)'
		});
	}

	function report(nodes: GridStackNode[]) {
		// gridstack also emits change when it re-columns the grid or a layout is
		// replaced; a save is taken only from a drag or a resize by the editor
		if (!onchange || !grid || !editing) return;
		const scale = REFERENCE_COLUMNS / grid.getColumn();
		const moved = nodes.flatMap((node) => {
			if (node.id == null || node.x == null || node.y == null || node.w == null) return [];
			return [
				{
					id: String(node.id),
					x: Math.round(node.x * scale),
					y: node.y,
					w: Math.max(1, Math.round(node.w * scale)),
					h: node.h ?? 1
				}
			];
		});
		onchange(moved);
	}

	const structure = $derived(widgets.map((w) => w.id).join('|'));

	$effect(() => {
		const key = structure;
		if (!host || key === '') return;
		const { columns, cellHeight } = metrics(host.clientWidth);
		const instance = GridStack.init(
			{
				column: REFERENCE_COLUMNS,
				cellHeight,
				margin: 0,
				mode: 'top',
				animate: true,
				staticGrid: !editing,
				alwaysShowResizeHandle: true,
				draggable: { handle: '.home-drag' },
				resizable: { handles: 'se' }
			},
			host
		);
		if (!instance) return;
		grid = instance;
		if (columns !== REFERENCE_COLUMNS) instance.column(columns, 'moveScale');
		// a gesture is the only thing that may write back: the drag and resize
		// stops carry the element that moved, a column change carries none
		const save = (el: GridItemHTMLElement) => {
			const node = el.gridstackNode;
			if (node) report([node]);
		};
		instance.on('drag', (_event, el) => {
			dragFrom = el.getBoundingClientRect();
		});
		instance.on('dragstop', (_event, el) => {
			flip(el);
			save(el);
		});
		instance.on('resizestop', (_event, el) => save(el));
		return () => {
			instance.destroy(false);
			grid = null;
		};
	});

	// setStatic re-reads the elements and drops the bounds parsed at init, so the
	// cells are re-applied after every unlock and whenever the layout is replaced
	function apply(wanted: WidgetPlacement[]) {
		const instance = grid;
		if (!instance) return;
		for (const want of wanted) {
			const node = instance.engine.nodes.find((each) => String(each.id) === want.id);
			if (!node?.el) continue;
			const same =
				node.x === want.x &&
				node.y === want.y &&
				node.w === want.w &&
				node.h === want.h &&
				node.minW === want.minW &&
				node.minH === want.minH &&
				node.maxW === want.maxW &&
				node.maxH === want.maxH;
			if (same) continue;
			instance.update(node.el, {
				x: want.x,
				y: want.y,
				w: want.w,
				h: want.h,
				minW: want.minW,
				minH: want.minH,
				maxW: want.maxW,
				maxH: want.maxH
			});
		}
	}

	$effect(() => {
		const on = editing;
		if (!grid) return;
		grid.setStatic(!on);
		apply(widgets);
	});

	$effect(() => {
		const wanted = widgets;
		if (!grid) return;
		apply(wanted);
	});

	$effect(() => {
		if (!host) return;
		const observer = new ResizeObserver(() => {
			if (!host || !grid) return;
			const { columns, cellHeight } = metrics(host.clientWidth);
			grid.column(columns, 'moveScale');
			grid.cellHeight(cellHeight);
			host.style.setProperty('--home-columns', String(columns));
			host.style.setProperty('--home-cell', `${cellHeight}px`);
		});
		observer.observe(host);
		return () => observer.disconnect();
	});
</script>

<div class="grid-stack home-grid" class:is-editing={editing} bind:this={host}>
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

	/* the cell lattice, drawn only while the layout is unlocked */
	.home-grid.is-editing {
		--home-line: color-mix(in srgb, var(--color-base-content) 10%, transparent);
		background-image:
			repeating-linear-gradient(to right, var(--home-line) 0 1px, transparent 1px 100%),
			repeating-linear-gradient(to bottom, var(--home-line) 0 1px, transparent 1px 100%);
		background-size:
			calc(100% / var(--home-columns, 18)) 100%,
			100% var(--home-cell, 96px);
	}

	.home-grid:not(.is-editing) :global(.ui-resizable-handle) {
		display: none;
	}
</style>
