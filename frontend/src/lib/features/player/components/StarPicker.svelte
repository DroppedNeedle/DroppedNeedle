<!--
	A player button that opens a small bar of five stars above it. Picking a
	star hands its number to `onpick` and closes the bar; `clear` adds a
	button at the bar's start that hands back 0: a bin that removes a
	rating, or an Off switch. The bar closes on a click outside it and on
	Escape, which gives focus back to the button. `label` is the button's
	name and tooltip.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { Star, Trash2 } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		/** The button's accessible name. */
		label: string;
		/** The stars shown filled in the bar, 0 for none. */
		value: number;
		/** 1 to 5, or 0 from the off button. */
		onpick: (stars: number) => void;
		/** The button at the bar's start that picks 0, if any. */
		clear?: 'remove' | 'off';
		active?: boolean;
		disabled?: boolean;
		/** The button's face. */
		children: Snippet;
	}

	let {
		label,
		value,
		onpick,
		clear = undefined,
		active = false,
		disabled = false,
		children
	}: Props = $props();

	const STARS = [1, 2, 3, 4, 5];
	let open = $state(false);
	let preview = $state(0);
	let root: HTMLDivElement | undefined = $state();
	const shown = $derived(preview || value);
	const panelId = $props.id();

	function pick(stars: number): void {
		onpick(stars);
		open = false;
		preview = 0;
	}

	$effect(() => {
		if (!open) return;
		const away = (event: PointerEvent) => {
			if (root && !root.contains(event.target as Node)) open = false;
		};
		const escape = (event: KeyboardEvent) => {
			if (event.key !== 'Escape') return;
			open = false;
			root?.querySelector<HTMLButtonElement>('button')?.focus();
		};
		document.addEventListener('pointerdown', away);
		document.addEventListener('keydown', escape);
		return () => {
			document.removeEventListener('pointerdown', away);
			document.removeEventListener('keydown', escape);
		};
	});
</script>

<div class="relative inline-flex" bind:this={root}>
	<Button
		variant="ghost"
		pill
		{active}
		class="w-10 px-0"
		aria-label={label}
		title={label}
		aria-expanded={open}
		aria-controls={open ? panelId : undefined}
		{disabled}
		onclick={() => (open = !open)}
	>
		{@render children()}
	</Button>
	{#if open}
		<div
			id={panelId}
			role="dialog"
			aria-label={label}
			class="absolute bottom-full left-1/2 z-40 mb-2 flex -translate-x-1/2 items-center gap-0.5 rounded-box border border-base-300 bg-base-100 p-1 shadow-xl"
		>
			{#if clear === 'remove'}
				<IconButton
					icon={Trash2}
					shape="square"
					size="sm"
					label="Remove rating"
					class="mr-1"
					disabled={value === 0}
					onclick={() => pick(0)}
				/>
			{:else if clear === 'off'}
				<Button
					variant="ghost"
					size="xs"
					active={value === 0}
					aria-pressed={value === 0}
					title="Play every track"
					class="mr-1"
					onclick={() => pick(0)}>Off</Button
				>
			{/if}
			{#each STARS as star (star)}
				<IconButton
					icon={Star}
					shape="square"
					size="sm"
					label={star === 1 ? '1 star' : `${star} stars`}
					aria-pressed={star === value}
					class={star <= shown
						? '[&_svg]:size-5 [&_svg]:fill-warning [&_svg]:text-warning'
						: '[&_svg]:size-5 [&_svg]:text-base-content/30'}
					onclick={() => pick(star)}
					onmouseenter={() => (preview = star)}
					onmouseleave={() => (preview = 0)}
					onfocus={() => (preview = star)}
					onblur={() => (preview = 0)}
				/>
			{/each}
		</div>
	{/if}
</div>
