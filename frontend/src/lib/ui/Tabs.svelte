<!--
	A row of tabs and the panel they show. The tabs are one tab stop: arrow
	keys, Home and End move between them and show the one reached. Each tab
	may carry an icon before its label, coloured while selected, and a quiet
	count. The panel is named by its tab, so the content passed in reads as
	that tab's. `aside` sits in the row beside the tabs (a list's search and
	sort, which belong to every tab). The `caps` look is a strip of mono
	capitals, the current tab underlined in the title colour (a drawer's
	tabs). The `folder` look is a row of paper divider tabs standing up from
	a paper panel, the current one taller and joined to it, the others
	stepping down (a file's sections, each a taste); `tab` then draws what a
	tab holds in place of its label. `scroll` keeps the tabs on one line
	that scrolls sideways when it is wider than the screen, rather than
	wrapping.
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
		look?: 'box' | 'border' | 'caps' | 'folder';
		class?: string;
		/** Layout only, for the panel: a drawer's panel fills and scrolls. */
		panelClass?: string;
		aside?: Snippet;
		/** What a tab holds in place of its label: the tab, and whether it is current. */
		tab?: Snippet<[TabItem<T>, boolean]>;
		/** One line of tabs that scrolls sideways instead of wrapping. */
		scroll?: boolean;
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
		tab: tabContent,
		scroll = false,
		children
	}: Props = $props();

	const tabId = (id: string) => `${idPrefix}-tab-${id}`;
	const panelId = $derived(`${idPrefix}-panel`);
	const looks = {
		box: 'tabs tabs-box',
		border: 'tabs tabs-border',
		caps: 'dn-tabs-caps',
		folder: 'dn-tabs-folder'
	};
	const tabLooks = { box: 'tab', border: 'tab', caps: 'dn-tab-caps', folder: 'dn-tab-folder' };
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
		class="{looks[look]} w-fit max-w-full {scroll
			? 'dn-tabs-scroll flex-nowrap overflow-x-auto'
			: 'flex-wrap'} {aside ? '' : className}"
		onkeydown={move}
	>
		{#each tabs as tab, at (tab.id)}
			<button
				type="button"
				role="tab"
				id={tabId(tab.id)}
				class="{tabLooks[look]} gap-2 {tab.id === selected ? 'tab-active' : ''}"
				style:--tab-at={look === 'folder' ? at : undefined}
				aria-selected={tab.id === selected}
				aria-controls={panelId}
				tabindex={tab.id === selected ? 0 : -1}
				onclick={() => onselect(tab.id)}
			>
				{#if tabContent}{@render tabContent(tab, tab.id === selected)}{:else}{#if tab.icon}<tab.icon
							class="h-4 w-4"
							aria-hidden="true"
						/>{/if}{tab.label}{#if tab.count != null}&nbsp;<span
							class="font-mono text-xs tabular-nums opacity-80"
						>
							{tab.count.toLocaleString()}
						</span>{/if}{/if}
			</button>
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
	class={[look === 'folder' ? 'dn-tabs-folder__panel' : '', panelClass].filter(Boolean).join(' ') ||
		undefined}
>
	{@render children()}
</div>

<style>
	.dn-tabs-scroll > [role='tab'] {
		flex-shrink: 0;
		white-space: nowrap;
	}
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
	.dn-tabs-folder {
		display: flex;
		align-items: flex-start;
		gap: 0.375rem;
	}
	.dn-tab-folder {
		display: flex;
		flex-direction: column;
		align-items: stretch;
		gap: 0.4375rem;
		margin-top: calc(0.5rem + min(var(--tab-at, 0), 4) * 0.25rem);
		padding: 0.625rem 1rem;
		border-radius: 7px 7px 0 0;
		background-color: color-mix(in oklab, var(--dn-paper) 82%, var(--dn-paper-content));
		color: var(--dn-paper-content);
		text-align: left;
		cursor: pointer;
		transition: background-color 150ms;
	}
	.dn-tab-folder:hover {
		background-color: color-mix(in oklab, var(--dn-paper) 90%, var(--dn-paper-content));
	}
	.dn-tab-folder.tab-active {
		margin-top: 0;
		padding-top: 0.875rem;
		background-color: var(--dn-paper-white);
	}
	.dn-tab-folder:focus-visible {
		outline-offset: -3px;
	}
	.dn-tabs-folder__panel {
		border-radius: 0 7px 7px 7px;
		background-color: var(--dn-paper-white);
		color: var(--dn-paper-content);
		box-shadow: 0 6px 14px rgb(0 0 0 / 0.35);
	}
	:global([data-theme='light']) .dn-tabs-folder__panel {
		border: 1px solid var(--dn-paper-border);
		box-shadow: none;
	}
	:global([data-theme='light']) .dn-tab-folder {
		border: 1px solid var(--dn-paper-border);
		border-bottom: 0;
	}
	@media (forced-colors: active) {
		.dn-tab-caps.tab-active {
			border-bottom-color: Highlight;
		}
		.dn-tab-folder,
		.dn-tabs-folder__panel {
			border: 1px solid CanvasText;
		}
		.dn-tab-folder.tab-active {
			border-color: Highlight;
		}
	}
</style>
