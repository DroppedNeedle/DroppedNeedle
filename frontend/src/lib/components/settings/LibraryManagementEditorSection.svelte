<!--
	One collapsible section of the management profile editor: icon, title,
	an On/Off state and summary chips, then the section's controls.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { ChevronRight } from 'lucide-svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';

	interface Chip {
		label: string;
		active?: boolean;
	}

	interface Props {
		icon: KitIcon;
		title: string;
		subtitle: string;
		active: boolean;
		/** Replaces the On/Off state text. */
		state?: string;
		chips: Chip[];
		children: Snippet;
	}

	let { icon: Icon, title, subtitle, active, state, chips, children }: Props = $props();
</script>

<details class="management-editor-section" data-active={active}>
	<summary class="management-editor-summary">
		<span class="management-editor-icon"><Icon class="h-4 w-4" /></span>
		<span class="management-editor-summary__copy"
			><strong>{title}</strong><small>{subtitle}</small></span
		>
		<span class="management-editor-overview">
			<span class="management-editor-state" data-active={active}
				>{state ?? (active ? 'On' : 'Off')}</span
			>
			{#each chips as chip (chip.label)}
				<span class="management-editor-chip" data-active={chip.active}>{chip.label}</span>
			{/each}
		</span>
		<ChevronRight class="h-4 w-4 management-editor-chevron" />
	</summary>
	{@render children()}
</details>
