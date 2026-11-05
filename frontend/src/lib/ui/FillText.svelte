<!--
	A block of text that fills the height its flex parent gives it, never
	less than `lines` lines, and fades where it is cut. `expanded` shows all
	of it; `clipped` says whether anything is hidden, for a "Read more".
-->
<script lang="ts">
	interface Props {
		text: string;
		lines?: number;
		expanded?: boolean;
		clipped?: boolean;
		/** Layout and text styles only. */
		class?: string;
	}

	let {
		text,
		lines = 4,
		expanded = false,
		clipped = $bindable(false),
		class: className = ''
	}: Props = $props();

	let box = $state<HTMLElement | null>(null);

	// The box's height follows the card around it, so measure on every resize.
	$effect(() => {
		const el = box;
		void text;
		if (!el || expanded) return;
		const measure = () => {
			clipped = el.scrollHeight > el.clientHeight + 2;
		};
		measure();
		const observer = new ResizeObserver(measure);
		observer.observe(el);
		return () => observer.disconnect();
	});
</script>

<div
	bind:this={box}
	class="dn-fill-text {className}"
	class:dn-fill-text--open={expanded}
	class:dn-fill-text--cut={clipped && !expanded}
	style:--dn-fill-lines={lines}
>
	{text}
</div>

<style>
	.dn-fill-text {
		flex: 1 1 0;
		min-height: calc(var(--dn-fill-lines) * 1lh);
		overflow: hidden;
		line-height: 1.625;
	}
	.dn-fill-text--open {
		flex: none;
		overflow: visible;
	}
	.dn-fill-text--cut {
		mask-image: linear-gradient(to bottom, #000 calc(100% - 2.5lh), transparent);
	}
</style>
