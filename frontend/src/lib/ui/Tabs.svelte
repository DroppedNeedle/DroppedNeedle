<!--
	A row of tabs and the panel they show. The tabs are one tab stop: arrow
	keys, Home and End move between them and show the one reached. Each tab
	may carry a quiet count. The panel is named by its tab, so the content
	passed in reads as that tab's.
-->
<script lang="ts" module>
	export interface TabItem<V extends string = string> {
		id: V;
		label: string;
		/** A quiet count after the label; nothing when absent. */
		count?: number | null;
	}
</script>

<script lang="ts" generics="T extends string">
	import type { Snippet } from 'svelte';

	interface Props {
		tabs: TabItem<T>[];
		selected: T;
		onselect: (id: T) => void;
		/** Names the row for screen readers. */
		label: string;
		/** Keeps the ids of two rows on one page apart. */
		idPrefix: string;
		look?: 'box' | 'border';
		class?: string;
		children: Snippet;
	}

	let {
		tabs,
		selected,
		onselect,
		label,
		idPrefix,
		look = 'box',
		class: className = '',
		children
	}: Props = $props();

	const tabId = (id: string) => `${idPrefix}-tab-${id}`;
	const panelId = $derived(`${idPrefix}-panel`);
	let row = $state<HTMLDivElement | null>(null);

	function move(event: KeyboardEvent): void {
		const at = tabs.findIndex((tab) => tab.id === selected);
		const last = tabs.length - 1;
		const next =
			event.key === 'ArrowRight' || event.key === 'ArrowDown'
				? at >= last
					? 0
					: at + 1
				: event.key === 'ArrowLeft' || event.key === 'ArrowUp'
					? at <= 0
						? last
						: at - 1
					: event.key === 'Home'
						? 0
						: event.key === 'End'
							? last
							: null;
		if (next === null) return;
		event.preventDefault();
		const tab = tabs[next];
		if (!tab) return;
		onselect(tab.id);
		row?.querySelector<HTMLElement>(`#${CSS.escape(tabId(tab.id))}`)?.focus();
	}
</script>

<div
	bind:this={row}
	role="tablist"
	aria-label={label}
	tabindex="-1"
	class="tabs w-fit max-w-full flex-wrap {look === 'box' ? 'tabs-box' : 'tabs-border'} {className}"
	onkeydown={move}
>
	{#each tabs as tab (tab.id)}
		<button
			type="button"
			role="tab"
			id={tabId(tab.id)}
			class="tab gap-2 {tab.id === selected ? 'tab-active' : ''}"
			aria-selected={tab.id === selected}
			aria-controls={panelId}
			tabindex={tab.id === selected ? 0 : -1}
			onclick={() => onselect(tab.id)}
			>{tab.label}{#if tab.count != null}&nbsp;<span class="badge badge-sm badge-ghost tabular-nums"
					>{tab.count.toLocaleString()}</span
				>{/if}</button
		>
	{/each}
</div>
<div role="tabpanel" id={panelId} aria-labelledby={tabId(selected)} tabindex="-1">
	{@render children()}
</div>
