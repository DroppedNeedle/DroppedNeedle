<!--
	A row of tabs and the panel they show. The tabs are one tab stop: arrow
	keys, Home and End move between them and show the one reached. Each tab
	may carry an icon before its label, coloured while selected, and a quiet
	count. The panel is named by its tab, so the content passed in reads as
	that tab's. `aside` sits in the row beside the tabs (a list's search and
	sort, which belong to every tab). The `caps` look is a strip of mono
	capitals, the current tab underlined in the title colour (a drawer's
	tabs).
-->
<script lang="ts" module>
	import type { KitIcon } from './Button.svelte';

	export interface TabItem<V extends string = string> {
		id: V;
		label: string;
		/** A lucide icon before the label. */
		icon?: KitIcon;
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
		look?: 'box' | 'border' | 'caps';
		class?: string;
		/** Layout only, for the panel: a drawer's panel fills and scrolls. */
		panelClass?: string;
		aside?: Snippet;
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
		panelClass = '',
		aside,
		children
	}: Props = $props();

	const tabId = (id: string) => `${idPrefix}-tab-${id}`;
	const panelId = $derived(`${idPrefix}-panel`);
	const looks = { box: 'tabs tabs-box', border: 'tabs tabs-border', caps: 'dn-tabs-caps' };
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

{#snippet tablist()}
	<div
		bind:this={row}
		role="tablist"
		aria-label={label}
		tabindex="-1"
		class="{looks[look]} w-fit max-w-full flex-wrap {aside ? '' : className}"
		onkeydown={move}
	>
		{#each tabs as tab (tab.id)}
			<button
				type="button"
				role="tab"
				id={tabId(tab.id)}
				class="{look === 'caps' ? 'dn-tab-caps' : 'tab'} gap-2 {tab.id === selected
					? 'tab-active'
					: ''}"
				aria-selected={tab.id === selected}
				aria-controls={panelId}
				tabindex={tab.id === selected ? 0 : -1}
				onclick={() => onselect(tab.id)}
				>{#if tab.icon}<tab.icon
						class="h-4 w-4"
						aria-hidden="true"
					/>{/if}{tab.label}{#if tab.count != null}&nbsp;<span
						class="font-mono text-xs tabular-nums opacity-80">{tab.count.toLocaleString()}</span
					>{/if}</button
			>
		{/each}
	</div>
{/snippet}

{#if aside}
	<div class="flex flex-wrap items-center gap-3 {className}">
		{@render tablist()}
		{@render aside()}
	</div>
{:else}
	{@render tablist()}
{/if}
<div
	role="tabpanel"
	id={panelId}
	aria-labelledby={tabId(selected)}
	tabindex="-1"
	class={panelClass || undefined}
>
	{@render children()}
</div>

<style>
	.dn-tabs-caps {
		display: flex;
		gap: 1.375rem;
	}
	.dn-tab-caps {
		display: inline-flex;
		align-items: center;
		min-height: 3rem;
		border-block: 3px solid transparent;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--dn-muted);
		cursor: pointer;
		transition: color 150ms;
	}
	.dn-tab-caps:hover {
		color: var(--color-base-content);
	}
	.dn-tab-caps.tab-active {
		border-bottom-color: var(--dn-title);
		color: var(--color-base-content);
	}
	.dn-tab-caps:focus-visible {
		outline-offset: -2px;
	}
	@media (forced-colors: active) {
		.dn-tab-caps.tab-active {
			border-bottom-color: Highlight;
		}
	}
</style>
